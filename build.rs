fn main() -> std::io::Result<()> {
    tonic_build::compile_protos("./src/bash-agent.proto")
}
