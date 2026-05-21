use cersei::events::AgentEvent;
use cersei::tools::permissions::AllowAll;
use cersei::{Agent, OpenAi};
use std::time::Instant;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let provider = OpenAi::builder()
        .base_url("http://127.0.0.1:1234/v1")
        .api_key("")
        .model("qwen3.6-27b-mtplx-optimized-speed")
        .build()?;
    let agent = Agent::builder()
        .provider(provider)
        .tools(cersei::tools::coding())
        .system_prompt("Coding assistant, be concise.")
        .max_turns(5)
        .permission_policy(AllowAll)
        .working_dir(".")
        .build()?;

    // let mut stream = agent.run_stream("Fix the failing tests");
    // while let Some(event) = stream.next().await {
    //     match event {
    //         AgentEvent::TextDelta(t) => print!("{t}"),
    //         AgentEvent::ToolStart { name, .. } => eprintln!("\n[{name}]"),
    //         AgentEvent::ToolEnd { name, duration, .. } => {
    //             eprintln!("[{name} done in {}ms]", duration.as_millis());
    //         }
    //         AgentEvent::Complete(output) => {
    //             eprintln!(
    //                 "\n--- {} turns, {} tool calls ---",
    //                 output.turns,
    //                 output.tool_calls.len()
    //             );
    //             break;
    //         }
    //         _ => {}
    //     }
    // }

    let agent = std::sync::Arc::new(agent);
    let start = Instant::now();
    let mut stream = agent.run_stream("What files are in the current directory? List them.");

    let mut text_bytes = 0usize;
    let mut tool_count = 0u32;

    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::TextDelta(text) => {
                text_bytes += text.len();
                print!("{}", text);
            }
            AgentEvent::ThinkingDelta(text) => {
                eprint!("\x1b[2m{}\x1b[0m", text); // dim
            }
            AgentEvent::TurnStart { turn } => {
                eprintln!("\n\x1b[36m── Turn {} ──\x1b[0m", turn);
            }
            AgentEvent::ToolStart { name, .. } => {
                tool_count += 1;
                eprint!("\x1b[33m⚙ {}...\x1b[0m ", name);
            }
            AgentEvent::ToolEnd {
                name,
                duration,
                is_error,
                ..
            } => {
                let status = if is_error {
                    "\x1b[31m✗\x1b[0m"
                } else {
                    "\x1b[32m✓\x1b[0m"
                };
                eprintln!("{} ({}ms)", status, duration.as_millis());
            }
            AgentEvent::CostUpdate {
                cumulative_cost,
                input_tokens,
                output_tokens,
                ..
            } => {
                if cumulative_cost > 0.0 {
                    eprintln!(
                        "\x1b[2m  cost: ${:.4} | {}in/{}out tokens\x1b[0m",
                        cumulative_cost, input_tokens, output_tokens
                    );
                }
            }
            AgentEvent::TokenWarning { pct_used, .. } => {
                eprintln!("\x1b[31m⚠ Context {:.0}% full\x1b[0m", pct_used * 100.0);
            }
            AgentEvent::Complete(output) => {
                let elapsed = start.elapsed();
                println!("\n");
                println!("─── Stream Complete ───");
                println!("Time:        {:.2}s", elapsed.as_secs_f64());
                println!("Turns:       {}", output.turns);
                println!("Tool calls:  {}", tool_count);
                println!("Text bytes:  {}", text_bytes);
                println!("Input tok:   {}", output.usage.input_tokens);
                println!("Output tok:  {}", output.usage.output_tokens);
                break;
            }
            AgentEvent::Error(e) => {
                eprintln!("\n\x1b[31mError: {}\x1b[0m", e);
                break;
            }
            _ => {} // Ignore other events
        }
    }
    Ok(())
}
