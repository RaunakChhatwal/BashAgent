from pathlib import Path
import sys
import paramiko

def run_remote_command(ssh, command):
    print(f"Running command {command}.")
    _, stdout, stderr = ssh.exec_command(command)
    exit_status = stdout.channel.recv_exit_status()
    if exit_status != 0:
        raise RuntimeError(f"Exit status: {exit_status}\nError: {stderr.read().decode()}")

if len(sys.argv) != 2:
    print("Usage: python script.py <ip_address>")
    sys.exit(1)

ip_address = sys.argv[1]

ssh = paramiko.SSHClient()
ssh.set_missing_host_key_policy(paramiko.AutoAddPolicy())

with ssh:
    ssh.connect(ip_address, username="claude", password="mcdonalds")

    run_remote_command(ssh, "mkdir -p ~/misc/system/nixos")
    run_remote_command(ssh, "nixos-generate-config --show-hardware-config \
        > ~/misc/system/nixos/hardware-configuration.nix")

    with ssh.open_sftp() as sftp:
        system = Path('/home/claude/misc/system')
        sftp.put("./flake.lock", str(system/"flake.lock"))
        sftp.put("./sandbox/flake-system.nix", str(system/"flake.nix"))
        sftp.put("./sandbox/configuration.nix", str(system/"nixos/configuration.nix"))
        sftp.put("./sandbox/pipe-read-invoc-notify.patch", str(system/"nixos/pipe-read-invoc-notify.patch"))

        run_remote_command(ssh, "mkdir -p ~/.config/home-manager")
        home_manager_dir = Path('/home/claude/.config/home-manager')
        sftp.put("./flake.lock", str(home_manager_dir/"flake.lock"))
        sftp.put("./sandbox/flake-home.nix", str(home_manager_dir/"flake.nix"))
        sftp.put("./sandbox/home.nix", str(home_manager_dir/"home.nix"))

    imports = "imports = \\[ .\\/hardware-configuration.nix \\];"
    run_remote_command(ssh, f"sed -i 's/^  # {imports}/  {imports}/' "
        "~/misc/system/nixos/configuration.nix")
    # run_remote_command(ssh, "sudo nixos-rebuild switch --flake ~/misc/system")

    run_remote_command(ssh, "home-manager switch")