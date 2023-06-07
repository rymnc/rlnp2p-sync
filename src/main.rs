use dotenv::dotenv;
use hex_literal::hex;
use pinata_sdk::{PinByJson, PinataApi};
use rln::{
    circuit::Fr,
    public::RLN,
    utils::{bytes_le_to_fr, vec_fr_to_bytes_le},
};
use serde_json::json;
use std::io::Cursor;
use web3::{
    futures::StreamExt,
    types::{BlockNumber, FilterBuilder, H160, H256, U256},
};

const PINATA_API_KEY_ENV: &str = "PINATA_API_KEY";
const PINATA_SECRET_API_KEY_ENV: &str = "PINATA_SECRET_API_KEY";
const RPC_URL_ENV: &str = "RPC_URL";

#[tokio::main]
async fn main() -> web3::contract::Result<()> {
    dotenv().ok();

    let pinata_api_key = std::env::var(PINATA_API_KEY_ENV).expect("missing pinata api key");
    let pinata_secret_api_key =
        std::env::var(PINATA_SECRET_API_KEY_ENV).expect("missing pinata secret api key");
    let rpc_url = std::env::var(RPC_URL_ENV).expect("missing rpc url");

    let ipfs_client = PinataApi::new(&pinata_api_key, &pinata_secret_api_key).unwrap();

    if (ipfs_client.test_authentication().await).is_err() {
        panic!("invalid pinata credentials");
    }

    let rln_config = Cursor::new(
        json!({
            "resources_folder": "tree_height_20/"
        })
        .to_string(),
    );

    let web3 = web3::Web3::new(web3::transports::WebSocket::new(&rpc_url).await?);
    let chain_id = web3.eth().chain_id().await?;

    let mut rln = RLN::new(20, rln_config).unwrap();

    let base_filter = create_base_filter();

    let logs = fetch_logs(
        &web3,
        &base_filter,
        3594457.into(),
        Some(BlockNumber::Latest),
    )
    .await;

    if logs.is_empty() {
        panic!("no logs found");
    }

    let (start_index, vec_comm) = process_logs(&logs);
    rln.set_leaves_from(start_index, Cursor::new(vec_comm.as_slice()))
        .expect("failed to set leaves");
    dbg!("set historical logs");

    let mut sub = web3.eth_subscribe().subscribe_new_heads().await?;
    while let Some(log) = sub.next().await {
        let data = log.unwrap();

        let block_number = data.number.unwrap().as_u64();
        let block_hash = data.hash.unwrap();

        let filter = base_filter
            .clone()
            .from_block(BlockNumber::from(block_number))
            .to_block(BlockNumber::from(block_number));
        let logs = fetch_logs(&web3, &filter, block_number.into(), None).await;

        if logs.is_empty() {
            continue;
        }

        let (start_index, vec_comm) = process_logs(&logs);
        rln.set_leaves_from(start_index, Cursor::new(vec_comm.as_slice()))
            .expect("failed to set leaves");
        dbg!("inserted new leaves");

        let mut root_buffer = Cursor::new(Vec::<u8>::new());
        rln.get_root(&mut root_buffer).unwrap();
        let (root, _) = bytes_le_to_fr(&root_buffer.into_inner());

        dbg!(root);

        process_ipfs_data(
            &ipfs_client,
            root,
            start_index,
            vec_comm,
            block_number,
            block_hash,
            chain_id,
        )
        .await;
    }

    Ok(())
}

fn create_base_filter() -> FilterBuilder {
    FilterBuilder::default()
        .address(vec![H160::from(hex!(
            "B8144E3214080f179D037bbb4dcaaa6B87f224E4"
        ))])
        .topics(
            Some(vec![hex!(
                "5a92c2530f207992057b9c3e544108ffce3beda4a63719f316967c49bf6159d2"
            )
            .into()]),
            None,
            None,
            None,
        )
}

async fn fetch_logs(
    web3: &web3::Web3<web3::transports::WebSocket>,
    filter: &FilterBuilder,
    block_number: BlockNumber,
    to_block: Option<BlockNumber>,
) -> Vec<web3::types::Log> {
    let logs_filter = filter
        .clone()
        .from_block(block_number)
        .to_block(to_block.unwrap_or(block_number))
        .build();

    match web3.eth().logs(logs_filter).await {
        Ok(logs) => logs,
        Err(err) => {
            eprintln!("Error fetching logs: {:?}", err);
            vec![]
        }
    }
}

fn process_logs(logs: &[web3::types::Log]) -> (usize, Vec<u8>) {
    let first_log = logs[0].clone();
    let start_index = U256::from_big_endian(&first_log.data.0[32..64]);
    let mut outputs = Vec::<Fr>::new();

    for log in logs {
        let data = &log.data.0;
        let id_commitment = U256::from_little_endian(&data[0..32]);

        let mut id_commitment_bytes = [0u8; 32];
        id_commitment.to_little_endian(&mut id_commitment_bytes);

        let (bytes_le, _) = bytes_le_to_fr(&id_commitment_bytes);
        outputs.push(bytes_le);
    }

    let vec_commitments = vec_fr_to_bytes_le(outputs.as_slice()).unwrap();

    (start_index.as_usize(), vec_commitments)
}

async fn process_ipfs_data(
    ipfs_client: &PinataApi,
    root: Fr,
    start_index: usize,
    insertions: Vec<u8>,
    block_number: u64,
    block_hash: H256,
    chain_id: U256,
) {
    let ipfs_data = json!({
        "state_change": "update",
        "insertions": insertions,
        "deletions": [], // todo
        "start_index": start_index,
        "root": format!("{}", root),
        "block_number": block_number,
        "block_hash": block_hash.to_string(),
        "chain_id": chain_id.as_u64(),
    });

    if let Ok(pinned_object) = ipfs_client.pin_json(PinByJson::new(ipfs_data)).await {
        let ipfs_hash = pinned_object.ipfs_hash;
        dbg!(ipfs_hash);
    }
}
