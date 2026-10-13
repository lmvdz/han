//! The in-sandbox shim: `listen` forwards loopback to the session pipe or socket; `mcp-stub` tunnels MCP
//! stdio to han-mcp outside the sandbox. See `han_host::shim`.

fn main() {
    std::process::exit(han_host::shim::main(std::env::args().skip(1).collect()));
}
