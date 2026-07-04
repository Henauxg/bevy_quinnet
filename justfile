set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

default:
    @just --list

# Mini local CI
check:
    @echo "==> fmt"
    cargo fmt --all -- --check
    @echo "==> clippy"
    cargo clippy --workspace --all-features --locked -- -D warnings
    @echo "==> checking workspace"
    cargo check --workspace --all-features --locked
    @echo "==> testing library"
    cargo test -p bevy_quinnet --all-features --locked
    @echo "==> docs"
    cargo doc -p bevy_quinnet --no-deps --all-features --locked
    @echo "==> machete"
    cargo machete

# Format all Rust code
format:
    cargo fmt --all

chat-server:
    cargo run -p bevy-quinnet-chat --bin chat-server

chat-client:
    cargo run -p bevy-quinnet-chat --bin chat-client

breakout:
    cargo run -p bevy-quinnet-breakout

# Launch all examples for manual smoke testing
[windows]
run-examples:
    $root = '{{justfile_directory()}}'
    Start-Process powershell -ArgumentList '-NoExit', '-Command', "Set-Location '$root'; cargo run -p bevy-quinnet-chat --bin chat-server"
    Start-Sleep -Seconds 3
    Start-Process powershell -ArgumentList '-NoExit', '-Command', "Set-Location '$root'; cargo run -p bevy-quinnet-chat --bin chat-client"
    Start-Process powershell -ArgumentList '-NoExit', '-Command', "Set-Location '$root'; cargo run -p bevy-quinnet-breakout"
    Start-Process powershell -ArgumentList '-NoExit', '-Command', "Set-Location '$root'; cargo run -p bevy-quinnet-breakout"
    Write-Host ''
    Write-Host 'Chat: type in the chat-client window.'
    Write-Host 'Breakout: click Host in one window, Join in the other.'

[unix]
run-examples:
    @echo 'Run these in separate terminals:'
    @echo '  just chat-server'
    @echo '  just chat-client'
    @echo '  just breakout   # run twice (Host + Join)'
