set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

chat_server_cmd := "cargo run -p bevy-quinnet-chat --bin chat-server"
chat_client_cmd := "cargo run -p bevy-quinnet-chat --bin chat-client"
breakout_cmd := "cargo run -p bevy-quinnet-breakout"

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

# Open a command in a new terminal window (Linux/macOS)
[unix]
[private]
_unix-terminal command:
    #!/usr/bin/env bash
    set -euo pipefail
    root='{{justfile_directory()}}'
    command='{{command}}'
    if command -v gnome-terminal &>/dev/null; then
        gnome-terminal -- bash -c "cd '$root' && $command; exec bash"
    elif command -v open &>/dev/null && [[ "$(uname)" == "Darwin" ]]; then
        open -na Terminal --args bash -c "cd '$root' && $command; exec bash"
    else
        exit 1
    fi

# Launch chat server and N clients in separate windows (default: 1 client)
[windows]
chat clients='1':
    $root = '{{justfile_directory()}}'
    Start-Process powershell -ArgumentList '-NoExit', '-Command', "Set-Location '$root'; {{chat_server_cmd}}"
    Start-Sleep -Seconds 2
    1..{{clients}} | ForEach-Object { Start-Process powershell -ArgumentList '-NoExit', '-Command', "Set-Location '$root'; {{chat_client_cmd}}" }
    Write-Host "Chat: launched {{clients}} client(s). Type in the chat-client windows."

[unix]
chat clients='1':
    #!/usr/bin/env bash
    set -euo pipefail
    clients='{{clients}}'
    if ! just _unix-terminal "{{chat_server_cmd}}"; then
        echo 'Run these in separate terminals:'
        echo '  {{chat_server_cmd}}'
        for ((i = 1; i <= clients; i++)); do
            echo '  {{chat_client_cmd}}'
        done
        exit 0
    fi
    sleep 2
    for ((i = 1; i <= clients; i++)); do
        just _unix-terminal "{{chat_client_cmd}}"
    done

# Launch two breakout windows (Host in one, Join in the other)
[windows]
breakout:
    $root = '{{justfile_directory()}}'
    Start-Process powershell -ArgumentList '-NoExit', '-Command', "Set-Location '$root'; {{breakout_cmd}}"
    Start-Process powershell -ArgumentList '-NoExit', '-Command', "Set-Location '$root'; {{breakout_cmd}}"
    Write-Host 'Breakout: click Host in one window, Join in the other.'

[unix]
breakout:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! just _unix-terminal "{{breakout_cmd}}"; then
        echo 'Run this in two separate terminals:'
        echo '  {{breakout_cmd}}'
        exit 0
    fi
    just _unix-terminal "{{breakout_cmd}}"