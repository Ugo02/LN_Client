//! # LNURL Client
//!
//! A command-line client for Lightning Network (LN) URL protocols. It talks to a local
//! Core Lightning (CLN) node via Unix socket RPC and performs HTTP requests to LNURL
//! servers for:
//! - **Channel request**: open an inbound channel with a remote LNURL service
//! - **Withdraw request**: receive a payment from a service by providing a BOLT11 invoice
//! - **Auth request**: prove ownership of the node by signing a challenge (LNURL-auth style)
//!
//! The server base URL can be given as a full URL or as `host:port` (IPv4/IPv6).

use serde::Deserialize;
use cln_rpc::ClnRpc;
use url::Url;
use anyhow::{Context, Result, anyhow};
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::str::FromStr;
use std::time::Duration;
use secp256k1::PublicKey;

// -----------------------------------------------------------------------------
// Configuration
// -----------------------------------------------------------------------------

/// HTTP timeout for requests to LNURL servers (connect + read).
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);

/// Returns the path to the Core Lightning RPC socket.
/// Uses `CLN_RPC_PATH` if set, otherwise a default path for testnet4.
fn get_cln_rpc_path() -> String {
    std::env::var("CLN_RPC_PATH")
        .unwrap_or_else(|_| "/home/ugo/.lightning/testnet4/lightning-rpc".to_string())
}

// -----------------------------------------------------------------------------
// CLI: commands and argument parsing
// -----------------------------------------------------------------------------

/// Supported subcommands and their arguments.
#[derive(Debug)]
enum Commands {
    RequestChannel {
        url: Url,
    },
    RequestWithdraw {
        url: Url,
        amount_msat: u64,
        description: Option<String>,
    },
    RequestAuth {
        url: Url,
    }
}

/// Prints usage to stderr.
fn print_usage() {
    eprintln!("Usage:");
    eprintln!("  lnurl-client request-channel <url|ip>");
    eprintln!("  lnurl-client request-withdraw <url|ip> <amount_msat> [description]");
    eprintln!("  lnurl-client request-auth <url|ip>");
}

/// Parses a string as a URL or as a host:port (IPv4 or IPv6).
/// Plain host:port is turned into `http://host:port`.
fn parse_url_or_ip(input: &str) -> Result<Url> {
    if let Ok(url) = Url::parse(input) {
        return Ok(url);
    }

    // IPv6 with port in brackets, e.g. [::1]:8080
    if let Some(bracket_end) = input.find("]:") {
        if input.starts_with('[') {
            let ip_part = &input[1..bracket_end];
            let port_part = &input[bracket_end + 2..];
            if port_part.parse::<u16>().is_ok() {
                if let Ok(ip) = IpAddr::from_str(ip_part) {
                    let url_str = format!("http://[{}]:{}", ip, port_part);
                    return Url::parse(&url_str)
                        .context("Failed to convert IP address with port to URL");
                }
            }
        }
    }

    // IPv4 or IPv6 with port, e.g. 192.168.1.1:8080
    if let Some(colon_pos) = input.rfind(':') {
        let ip_part = &input[..colon_pos];
        let port_part = &input[colon_pos + 1..];
        
        if port_part.parse::<u16>().is_ok() {
            if let Ok(ip) = IpAddr::from_str(ip_part) {
                let url_str = format!("http://{}:{}", ip, port_part);
                return Url::parse(&url_str)
                    .context("Failed to convert IP address with port to URL");
            }
        }
    }

    // Plain IP (no port); default to http with no port
    if let Ok(ip) = IpAddr::from_str(input) {
        let url_str = format!("http://{}", ip);
        return Url::parse(&url_str)
            .context("Failed to convert IP address to URL");
    }
    
    Err(anyhow!("Invalid URL or IP address: {}", input))
}

/// Parses command-line arguments into a `Commands` variant.
fn parse_args() -> Result<Commands> {
    let args: Vec<String> = std::env::args().collect();
    
    if args.len() < 2 {
        print_usage();
        return Err(anyhow!("No command provided"));
    }

    let command_name = args[1].as_str();
    
    match command_name {
        "request-channel" => {
            if args.len() < 3 {
                return Err(anyhow!("request-channel requires a <url> argument"));
            } else if args.len() > 3 {
                return Err(anyhow!("request-channel does not accept additional arguments"));
            }
            
            let url = parse_url_or_ip(&args[2])?;

            Ok(Commands::RequestChannel {
                url,
            })
        } 
        "request-withdraw" => {
            if args.len() < 4 {
                return Err(anyhow!("request-withdraw requires <url> and <amount_msat> arguments"));
            } else if args.len() > 5 {
                return Err(anyhow!("request-withdraw accepts at most 3 arguments: <url> <amount_msat> [description]"));
            }
            
            let url = parse_url_or_ip(&args[2])?;
            let amount_msat = args[3].parse::<u64>()
                .context("amount_msat must be a valid number")?;
            let description = if args.len() == 5 {
                Some(args[4].clone())
            } else {
                None
            };

            Ok(Commands::RequestWithdraw {
                url,
                amount_msat,
                description,
            })
        }
        "request-auth" | "lnurl-auth" => {
            if args.len() < 3 {
                return Err(anyhow!("request-auth requires a <url> argument"));
            } else if args.len() > 3 {
                return Err(anyhow!("request-auth does not accept additional arguments"));
            }
            
            let url = parse_url_or_ip(&args[2])?;

            Ok(Commands::RequestAuth {
                url,
            })
        }
        _ => {
            print_usage();
            Err(anyhow!("Unknown command: {}", command_name))
        }
    }
}

// -----------------------------------------------------------------------------
// Lightning RPC helpers
// -----------------------------------------------------------------------------

/// Fetches the local node's pubkey and builds a node URI (pubkey@host:port)
/// for use in channel-open callbacks. Host is fixed to 127.0.0.1:49735.
fn get_node_uri(ln_client: &mut ClnRpc, rt: &tokio::runtime::Runtime) -> Result<String> {
    let node_info = rt.block_on(ln_client.call(cln_rpc::Request::Getinfo(cln_rpc::model::requests::GetinfoRequest{})));
    let node_uri = match node_info {
        Ok(cln_rpc::model::Response::Getinfo(response)) => {
            let pubkey = response.id.to_string();
            println!("Node pubkey initialized: {}", pubkey);
            format!("{}@{}", pubkey, "127.0.0.1:49735")
        }
        Err(e) => {
            return Err(anyhow!("Failed to get node info: {}", e));
        }
        _ => {
            return Err(anyhow!("Unexpected response type"));
        }
    };

    Ok(node_uri)
}

/// Connects the local CLN node to a remote node given a URI `pubkey@host:port`.
fn connect_to_node(ln_client: &mut ClnRpc, rt: &tokio::runtime::Runtime, node_uri: &str) -> Result<()> {
    let parsed = node_uri.split('@').collect::<Vec<&str>>();
    if parsed.len() != 2 {
        return Err(anyhow!("Invalid node URI: {}", node_uri));
    }
    let pubkey = PublicKey::from_str(parsed[0])?;
    let host = parsed[1];
    let port = host.split(':').collect::<Vec<&str>>()[1];
    let ip_addr: Ipv4Addr = host.split(':').collect::<Vec<&str>>()[0].parse()?;

    println!("Connecting to node {}@{}:{}...", pubkey, ip_addr, port);
    let request = cln_rpc::model::requests::ConnectRequest{
        id: pubkey.to_string(),
        host: Some(ip_addr.to_string()),
        port: port.parse::<u16>().ok(),
    };

    let _response = rt.block_on(ln_client.call(cln_rpc::Request::Connect(request)))?;

    Ok(())
}

// -----------------------------------------------------------------------------
// Channel request (LNURL channel open)
// -----------------------------------------------------------------------------

/// Response from GET /request-channel (LNURL channel open parameters).
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ChannelRequestResponse {
    uri: String,
    callback: String,
    k1: String,
    tag: String,
}

/// Response from the channel-open callback (remoteid + k1).
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ChannelOpenResponse {
    status: String,
    reason: Option<String>,
    txid: Option<String>,
    channel_id: Option<String>,
}

/// Performs the LNURL channel-open flow: get params, connect to remote node,
/// then call the open-channel callback with our pubkey and k1.
fn channel_request(url: &Url) -> Result<()> {
    println!("Requesting channel info from {}...", url);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .context("Failed to create Tokio runtime")?;
    let cln_rpc_path = get_cln_rpc_path();
    let mut ln_client = rt.block_on(cln_rpc::ClnRpc::new(&cln_rpc_path))?;

    let node_uri = get_node_uri(&mut ln_client, &rt)?;

    println!("Node URI: {}", node_uri);

    let request_url = format!("{}/request-channel", url.as_str().trim_end_matches('/'));
    let resp: ChannelRequestResponse = ureq::get(&request_url)
        .timeout(HTTP_TIMEOUT)
        .call()
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("timed out") || msg.contains("connection") {
                anyhow!(
                    "{}. Check: same network (e.g. 192.168.x.x), firewall, server listening on 0.0.0.0:{}",
                    msg,
                    url.port_or_known_default().unwrap_or(80)
                )
            } else {
                anyhow!("{}", msg)
            }
        })?
        .into_json()?;
    
    println!("Received channel request:");
    println!("  URI: {}", resp.uri);
    println!("  Callback: {}", resp.callback);
    println!("  k1: {}", resp.k1);

    connect_to_node(&mut ln_client, &rt, &resp.uri)?;

    println!("Requesting channel open...");

    // node_uri is "pubkey@host:port"; callback expects remoteid = pubkey only.
    let pubkey = node_uri.split('@').next()
        .ok_or_else(|| anyhow!("Invalid node URI format"))?;
    let open_url = format!(
        "{}?remoteid={}&k1={}",
        resp.callback,
        pubkey,
        resp.k1
    );
    println!("Open URL: {}", open_url);
    
    let open_resp = match ureq::get(&open_url).call() {
        Ok(resp) => resp.into_json::<ChannelOpenResponse>()?,
        Err(e) => {
            return Err(anyhow!("Failed to open channel: {}", e));
        }
    };
    println!("Open response: {:?}", open_resp);
     
    println!("Channel opened successfully!");
    if let Some(txid) = open_resp.txid {
        println!("  Transaction ID: {}", txid);
    }
    if let Some(channel_id) = open_resp.channel_id {
        println!("  Channel ID: {}", channel_id);
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// Withdraw request (LNURL withdraw)
// -----------------------------------------------------------------------------

/// Response from GET /request-withdraw (withdraw parameters and limits).
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct WithdrawRequestResponse {
    callback: String,
    k1: String,
    tag: String,
    #[serde(rename = "defaultDescription")]
    default_description: String,
    #[serde(rename = "minWithdrawable")]
    min_withdrawable: u64,
    #[serde(rename = "maxWithdrawable")]
    max_withdrawable: u64,
}

/// Response from the withdraw callback (status and optional reason).
#[derive(Debug, Deserialize)]
struct WithdrawResponse {
    status: String,
    reason: Option<String>,
}

/// LNURL withdraw flow: get withdraw params, create a BOLT11 invoice for the
/// requested amount, then call the withdraw callback with k1 and the invoice (pr).
fn withdraw_request(url: &Url, amount_msat: u64, description: Option<String>) -> Result<()> {
    println!("Requesting withdrawal info from {}...", url);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .context("Failed to create Tokio runtime")?;
    let cln_rpc_path = get_cln_rpc_path();
    let mut ln_client = rt.block_on(cln_rpc::ClnRpc::new(&cln_rpc_path))?;

    let request_url = format!("{}/request-withdraw", url.as_str().trim_end_matches('/'));
    let resp: WithdrawRequestResponse = ureq::get(&request_url).call()?.into_json()?;
    
    println!("Received withdraw request:");
    println!("  Callback: {}", resp.callback);
    println!("  k1: {}", resp.k1);
    println!("  Min withdrawable: {} msat", resp.min_withdrawable);
    println!("  Max withdrawable: {} msat", resp.max_withdrawable);
    println!("  Default description: {}", resp.default_description);

    if amount_msat < resp.min_withdrawable || amount_msat > resp.max_withdrawable {
        return Err(anyhow!(
            "Amount {} msat is outside allowed range [{}, {}]",
            amount_msat,
            resp.min_withdrawable,
            resp.max_withdrawable
        ));
    }

    let description = description.unwrap_or_else(|| resp.default_description.clone());
    println!("Creating invoice for {} msat with description: {}...", amount_msat, description);

    // Create a BOLT11 invoice via CLN so the server can pay us.
    let label = format!("lnurl-withdraw-{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis());
    
    let invoice_request = cln_rpc::model::requests::InvoiceRequest {
        amount_msat: cln_rpc::primitives::AmountOrAny::Amount(
            cln_rpc::primitives::Amount::from_msat(amount_msat)
        ),
        description: description.clone(),
        label: label.clone(),
        expiry: None,
        fallbacks: None,
        preimage: None,
        cltv: None,
        deschashonly: None,
        exposeprivatechannels: None,
    };

    let invoice_response = rt.block_on(ln_client.call(cln_rpc::Request::Invoice(invoice_request)))?;
    
    let bolt11 = match invoice_response {
        cln_rpc::model::Response::Invoice(resp) => resp.bolt11.to_string(),
        _ => return Err(anyhow!("Unexpected response type from invoice request")),
    };

    println!("Invoice created: {}...", &bolt11[..50.min(bolt11.len())]);

    println!("Submitting withdrawal request...");
    let withdraw_url = format!(
        "{}?k1={}&pr={}",
        resp.callback,
        resp.k1,
        urlencoding::encode(&bolt11)
    );
    
    let http_resp = ureq::get(&withdraw_url).call();
    let withdraw_resp = match http_resp {
        Ok(r) => r.into_json::<WithdrawResponse>()?,
        Err(ureq::Error::Status(code, r)) => {
            // Surface server error body (e.g. payment failure reason).
            let body = r.into_string().unwrap_or_else(|_| "(no body)".to_string());
            return Err(anyhow!(
                "Withdraw request failed (HTTP {}): {}",
                code,
                body
            ));
        }
        Err(e) => return Err(anyhow!("Withdraw request failed: {}", e)),
    };

    println!("Withdraw response: {:?}", withdraw_resp);

    if withdraw_resp.status == "OK" {
        println!("Withdrawal successful! Payment received.");
    } else {
        return Err(anyhow!(
            "Withdrawal failed: {}",
            withdraw_resp.reason.unwrap_or_else(|| "Unknown error".to_string())
        ));
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// Auth request (LNURL-auth style: challenge + signed response)
// -----------------------------------------------------------------------------

/// Response from the auth callback (status and optional reason).
#[derive(Debug, Deserialize)]
struct AuthResponse {
    status: String,
    reason: Option<String>,
}

/// Parses the k1 challenge from the server response.
/// Accepts either a JSON object `{"k1":"<hex>"}` or a raw hex string (e.g. from GET /auth-challenge).
fn parse_k1_from_challenge(body: &str) -> Result<String> {
    let body = body.trim();
    if body.starts_with('{') {
        #[derive(Deserialize)]
        struct K1Response { k1: String }
        Ok(serde_json::from_str::<K1Response>(body)?.k1)
    } else {
        Ok(body.to_string())
    }
}

/// LNURL-auth style flow: GET /auth-challenge for k1, sign k1 with the local node
/// (CLN signmessage), then GET /auth-response with k1, signature (zbase from CLN), and pubkey.
/// The server verifies the signature (e.g. via CLN checkmessage).
fn auth_request(url: &Url) -> Result<()> {
    let base = url.as_str().trim_end_matches('/');
    println!("Requesting auth challenge from {}...", base);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .context("Failed to create Tokio runtime")?;
    let cln_rpc_path = get_cln_rpc_path();
    let mut ln_client = rt.block_on(cln_rpc::ClnRpc::new(&cln_rpc_path))?;

    let node_info = rt.block_on(ln_client.call(cln_rpc::Request::Getinfo(cln_rpc::model::requests::GetinfoRequest {})))?;
    let pubkey = match node_info {
        cln_rpc::model::Response::Getinfo(response) => response.id.to_string(),
        _ => return Err(anyhow!("Unexpected response type from getinfo")),
    };
    println!("Node pubkey: {}", pubkey);

    let challenge_url = format!("{}/auth-challenge", base);
    let resp = ureq::get(&challenge_url).call()?;
    let body = resp.into_string()?;
    let k1 = parse_k1_from_challenge(&body)?;
    println!("Received k1: {}", k1);

    println!("Signing challenge...");
    let sign_request = cln_rpc::model::requests::SignmessageRequest {
        message: k1.clone(),
    };
    let sign_response = rt.block_on(ln_client.call(cln_rpc::Request::SignMessage(sign_request)))?;
    // Use CLN's zbase field directly; server expects this format (e.g. for checkmessage).
    let signature = match sign_response {
        cln_rpc::model::Response::SignMessage(r) => r.zbase.to_string(),
        _ => return Err(anyhow!("Unexpected response type from signmessage")),
    };
    println!("Signature (zbase): {}...", &signature[..signature.len().min(24)]);

    let response_url = format!(
        "{}/auth-response?k1={}&signature={}&pubkey={}",
        base,
        urlencoding::encode(&k1),
        urlencoding::encode(&signature),
        urlencoding::encode(&pubkey)
    );
    println!("Submitting auth response...");

    let http_resp = ureq::get(&response_url).call();
    let auth_resp = match http_resp {
        Ok(r) => r.into_json::<AuthResponse>()?,
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_else(|_| "(no body)".to_string());
            return Err(anyhow!(
                "Auth response failed (HTTP {}): {}",
                code,
                body
            ));
        }
        Err(e) => return Err(anyhow!("Auth request failed: {}", e)),
    };

    if auth_resp.status == "OK" {
        println!("Authentication successful!");
    } else {
        return Err(anyhow!(
            "Authentication failed: {}",
            auth_resp.reason.unwrap_or_else(|| "Unknown error".to_string())
        ));
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// Entry point
// -----------------------------------------------------------------------------

/// Parses CLI, runs the chosen LNURL command, and exits with an appropriate code.
fn main() {
    let command = match parse_args() {
        Ok(command) => command,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };
 

    let result = match command {
        Commands::RequestChannel { url } => {
            channel_request(&url)
        }
        Commands::RequestWithdraw { url, amount_msat, description } => {
            withdraw_request(&url, amount_msat, description)
        }
        Commands::RequestAuth { url } => {
            auth_request(&url)
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}