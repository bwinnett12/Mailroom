use reqwest::Client;
use serde_json::json;
use std::fs;
use std::env;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <infile> <outfile>", args[0]);
        std::process::exit(1);
    }

    let infile = &args[1];
    let outfile = &args[2];

    // Read input file
    let content = fs::read_to_string(infile)
        .expect("Failed to read input file");

    // Call LocalAI
    let client = Client::new();
    let response = client
        .post("http://loom.tail4b1127.ts.net/v1/chat/completions")
        .json(&json!({
            "model": "qwen_qwen3.5-0.8b",
            "messages": [
                {
                    "role": "user",
                    "content": format!("Clean up this markdown. Fix formatting, remove boilerplate, organize sections clearly:\n\n{}", content)
                }
            ]
        }))
        .send()
        .await
        .expect("Failed to call LocalAI");

    let body = response.json::<serde_json::Value>()
        .await
        .expect("Failed to parse response");

    let cleaned = body["choices"][0]["message"]["content"]
        .as_str()
        .expect("Failed to extract content");

    // Write output file
    fs::write(outfile, cleaned)
        .expect("Failed to write output file");

    println!("✓ Cleaned: {} → {}", infile, outfile);
}
