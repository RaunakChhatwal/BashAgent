{ pkgs, bash-agent, ... }:
let patched-kernel = pkgs.linuxPackages_latest.kernel.override {
  kernelPatches = [{
    name = "An ioctl command to wait until a task blocks to read a pipe.";
    patch = ./pipe-read-invoc-notify.patch;
  }];
};
in {
  # imports = [ ./hardware-configuration.nix ];

  boot.loader.grub.enable = true;
  boot.loader.grub.device = "/dev/vda";
  boot.loader.grub.useOSProber = true;

  boot.kernelPackages = pkgs.linuxPackagesFor patched-kernel;
  boot.kernelParams = [];

  networking.hostName = "nixos"; # Define your hostname.

  # Enable networking
  networking.networkmanager.enable = true;

  time.timeZone = "America/New_York";

  # Select internationalisation properties.
  i18n.defaultLocale = "en_US.UTF-8";
  i18n.extraLocaleSettings = {
    LC_ADDRESS = "en_US.UTF-8";
    LC_IDENTIFICATION = "en_US.UTF-8";
    LC_MEASUREMENT = "en_US.UTF-8";
    LC_MONETARY = "en_US.UTF-8";
    LC_NAME = "en_US.UTF-8";
    LC_NUMERIC = "en_US.UTF-8";
    LC_PAPER = "en_US.UTF-8";
    LC_TELEPHONE = "en_US.UTF-8";
    LC_TIME = "en_US.UTF-8";
  };

  # Configure keymap in X11
  services.xserver.xkb = {
    variant = "";
    layout = "us";
  };
  
  users.users.root.initialPassword = "mcdonalds";
  users.users.claude = {
    isNormalUser = true;
    initialPassword = "mcdonalds";
    description = "Claude";
    extraGroups = [ "networkmanager" "wheel" ];
    packages = with pkgs; [];
  };

  security.sudo.extraRules = [{
    users = [ "claude" ];
    commands = [{
      command = "ALL";
      options = [ "NOPASSWD" ];
    }];
  }];

  environment.systemPackages = with pkgs; [ wget python3 home-manager bash-agent ];

  services.openssh = {
    enable = true;
    settings.PasswordAuthentication = true;
  };

  environment.variables.PKG_CONFIG_PATH = "/run/current-system/sw/lib/pkgconfig/";

  nix.settings.experimental-features = [ "nix-command" "flakes" ];

  virtualisation.docker.enable = true;

  # Open ports in the firewall.
  networking.nftables.enable = true;
  networking.firewall.enable = true;

  system.stateVersion = "25.05";  # Do not change this

}
