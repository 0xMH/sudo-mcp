// sudo-mcp is an MCP server that runs sudo commands via a native OS password
// dialog. The same binary doubles as the SUDO_ASKPASS helper: when invoked
// with SUDO_MCP_ROLE=askpass (sudo execs the path in SUDO_ASKPASS, we set
// it to our own path), it pops the dialog and writes the password to stdout.
// Passwords go from the dialog to sudo's stdin only -- never through the MCP
// transport, never into the model context.

use std::env;
use std::io::Write;
use std::process::ExitCode;

use anyhow::Result;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
use rmcp::service::ServiceExt;
use rmcp::transport::stdio;
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData, ServerHandler};
use serde::Deserialize;

mod prompt;
mod sudo;

#[cfg(unix)]
mod process;

const ROLE_ENV: &str = "SUDO_MCP_ROLE";
const REASON_ENV: &str = "SUDO_PROMPT_REASON";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SudoRunInput {
    /// command and arguments. Example: ["apt","install","-y","htop"]
    pub argv: Vec<String>,
    /// one-line justification shown in the password dialog. Example: "Install htop system-wide"
    pub reason: String,
    /// timeout in seconds (default 120, max 3600)
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// working directory (optional)
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Clone)]
pub struct SudoServer {
    self_path: String,
    #[allow(dead_code)]
    tool_router: ToolRouter<SudoServer>,
}

#[tool_router]
impl SudoServer {
    fn new(self_path: String) -> Self {
        Self {
            self_path,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Run a single command as root. The user is prompted for their password via a native OS dialog (SUDO_ASKPASS); the password never enters this conversation. Pass argv as a list, not a shell string. Always include a short `reason` so the user sees in the dialog what they are authorizing."
    )]
    async fn sudo_run(
        &self,
        Parameters(input): Parameters<SudoRunInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let text = sudo::run_sudo(input, &self.self_path)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }
}

#[tool_handler]
impl ServerHandler for SudoServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("sudo-mcp", env!("CARGO_PKG_VERSION")))
    }
}

fn main() -> ExitCode {
    if env::var(ROLE_ENV).as_deref() == Ok("askpass") {
        return run_askpass();
    }
    match run_server() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sudo-mcp: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_server() -> Result<()> {
    let self_path = env::current_exe()?.to_string_lossy().into_owned();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        let service = SudoServer::new(self_path).serve(stdio()).await?;
        service.waiting().await?;
        Ok::<_, anyhow::Error>(())
    })
}

// run_askpass is dispatched when the binary is exec'd by sudo as the
// SUDO_ASKPASS program. It receives sudo's prompt as argv[1], shows a
// native dialog, and writes the password to stdout. Failures (no GUI,
// user cancels) exit non-zero so sudo treats it as a failed auth.
fn run_askpass() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let mut prompt = args
        .get(1)
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| "Sudo password required".to_string());

    if let Ok(reason) = env::var(REASON_ENV) {
        if !reason.is_empty() {
            prompt.push_str("\n\nReason: ");
            prompt.push_str(&reason);
        }
    }

    match prompt::prompt_password(&prompt) {
        Ok(pw) => {
            // sudo trims trailing whitespace, so an extra newline is safe.
            let mut stdout = std::io::stdout().lock();
            if stdout.write_all(&pw).is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("sudo-mcp askpass: {e}");
            ExitCode::FAILURE
        }
    }
}
