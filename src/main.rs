use mind_mcp::{state, tools};

use rmcp::{ServiceExt, transport::stdio};
use tools::MindTools;

fn main() -> anyhow::Result<()> {
    let project = state::Project::resolve()?;

    // One-shot human commands. No args: serve MCP over stdio.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("show") => {
            println!("{}", tools::show_impl(&project, None, None)?);
            return Ok(());
        }
        Some("ready") => {
            println!("{}", tools::ready_impl(&project)?);
            return Ok(());
        }
        Some("graph") => {
            println!("{}", tools::show_impl(&project, None, Some("tree".into()))?);
            return Ok(());
        }
        Some(other) => {
            anyhow::bail!("unknown command '{other}'. use: show | ready | graph")
        }
        None => {}
    }

    tokio::runtime::Runtime::new()?.block_on(async {
        let service = MindTools { project }.serve(stdio()).await?;
        service.waiting().await?;
        Ok::<_, anyhow::Error>(())
    })
}
