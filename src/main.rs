use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Parser)]
#[command(name = "lnurl-client")]
#[command(about = "LNURL client for channel requests and withdrawals")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    ChannelRequest {
        server: String,
        #[arg(long, default_value = "lightning-cli")]
        cli_path: String,
        #[arg(long)]
        network: Option<String>,
    },

    WithdrawRequest {
        server: String,
        amount_msat: u64,
        #[arg(long, default_value = "LNURL withdrawal")]
        description: String,
        #[arg(long, default_value = "lightning-cli")]
        cli_path: String,
        #[arg(long)]
        network: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
struct ChannelRequestResponse {
    uri: String,
    callback: String,
    k1: String,
    tag: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WithdrawRequestResponse {
    callback: String,
    k1: String,
    tag: String,
    default_description: String,
    min_withdrawable: u64,
    max_withdrawable: u64,
}

#[derive(Debug, Deserialize)]
struct ChannelOpenResponse {
    status: String,
    reason: Option<String>,
    txid: Option<String>,
    channel_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WithdrawResponse {
    status: String,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GetInfoResponse {
    id: String,
}

fn build_cli_cmd(cli_path: &str, network: &Option<String>) -> Command {
    let mut cmd = Command::new(cli_path);
    if let Some(net) = network {
        cmd.arg(format!("--network={}", net));
    }
    cmd
}

fn get_local_node_id(cli_path: &str, network: &Option<String>) -> Result<String, Box<dyn std::error::Error>> {
    let output = build_cli_cmd(cli_path, network)
        .arg("getinfo")
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("getinfo failed: {}", stderr).into());
    }

    let info: GetInfoResponse = serde_json::from_slice(&output.stdout)?;
    Ok(info.id)
}

fn connect_to_node(cli_path: &str, network: &Option<String>, uri: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("Connecting to {}...", uri);
    
    let output = build_cli_cmd(cli_path, network)
        .arg("connect")
        .arg(uri)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // CLN returns error if already connected, which is fine
        if !stderr.contains("already connected") {
            return Err(format!("connect failed: {}", stderr).into());
        }
        println!("Already connected to peer");
    } else {
        println!("Successfully connected");
    }
    Ok(())
}

fn create_invoice(
    cli_path: &str,
    network: &Option<String>,
    amount_msat: u64,
    description: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let label = format!("lnurl-withdraw-{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis());

    let output = build_cli_cmd(cli_path, network)
        .arg("invoice")
        .arg(format!("{}msat", amount_msat))
        .arg(&label)
        .arg(description)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("invoice creation failed: {}", stderr).into());
    }

    #[derive(Deserialize)]
    struct InvoiceResponse {
        bolt11: String,
    }

    let resp: InvoiceResponse = serde_json::from_slice(&output.stdout)?;
    Ok(resp.bolt11)
}

async fn channel_request(server: &str, cli_path: &str, network: &Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    
    println!("Requesting channel info from {}...", server);
    let url = format!("{}/channel_request", server.trim_end_matches('/'));
    let resp: ChannelRequestResponse = client.get(&url).send().await?.json().await?;
    
    println!("Received channel request:");
    println!("  URI: {}", resp.uri);
    println!("  Callback: {}", resp.callback);
    println!("  k1: {}", resp.k1);

    connect_to_node(cli_path, network, &resp.uri)?;

    let local_node_id = get_local_node_id(cli_path, network)?;
    println!("Local node ID: {}", local_node_id);

    println!("Requesting channel open...");
    let open_url = format!(
        "{}/open-channel?remoteid={}&k1={}",
        server.trim_end_matches('/'),
        local_node_id,
        resp.k1
    );
    
    let open_resp: ChannelOpenResponse = client.get(&open_url).send().await?.json().await?;
    
    if open_resp.status == "OK" {
        println!("Channel opened successfully!");
        if let Some(txid) = open_resp.txid {
            println!("  Transaction ID: {}", txid);
        }
        if let Some(channel_id) = open_resp.channel_id {
            println!("  Channel ID: {}", channel_id);
        }
    } else {
        println!("Channel open failed: {}", open_resp.reason.unwrap_or_default());
    }

    Ok(())
}

async fn withdraw_request(
    server: &str,
    amount_msat: u64,
    description: &str,
    cli_path: &str,
    network: &Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    
    println!("Requesting withdrawal info from {}...", server);
    let url = format!("{}/withdraw-request", server.trim_end_matches('/'));
    let resp: WithdrawRequestResponse = client.get(&url).send().await?.json().await?;
    
    println!("Received withdraw request:");
    println!("  Min withdrawable: {} msat", resp.min_withdrawable);
    println!("  Max withdrawable: {} msat", resp.max_withdrawable);
    println!("  Default description: {}", resp.default_description);

    if amount_msat < resp.min_withdrawable || amount_msat > resp.max_withdrawable {
        return Err(format!(
            "Amount {} msat is outside allowed range [{}, {}]",
            amount_msat, resp.min_withdrawable, resp.max_withdrawable
        ).into());
    }

    println!("Creating invoice for {} msat...", amount_msat);
    let bolt11 = create_invoice(cli_path, network, amount_msat, description)?;
    println!("Invoice created: {}...", &bolt11[..50.min(bolt11.len())]);

    println!("Submitting withdrawal request...");
    let withdraw_url = format!(
        "{}/withdraw?k1={}&pr={}",
        server.trim_end_matches('/'),
        resp.k1,
        bolt11
    );
    
    let withdraw_resp: WithdrawResponse = client.get(&withdraw_url).send().await?.json().await?;
    
    if withdraw_resp.status == "OK" {
        println!("Withdrawal successful! Payment received.");
    } else {
        println!("Withdrawal failed: {}", withdraw_resp.reason.unwrap_or_default());
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::ChannelRequest { server, cli_path, network } => {
            channel_request(&server, &cli_path, &network).await
        }
        Commands::WithdrawRequest { server, amount_msat, description, cli_path, network } => {
            withdraw_request(&server, amount_msat, &description, &cli_path, &network).await
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
