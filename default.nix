let rust-overlay =
  builtins.fetchTarball "https://github.com/oxalica/rust-overlay/archive/master.tar.gz";
in { pkgs ? import <nixpkgs> { overlays = [ (import rust-overlay) ]; }}:
let
  toolchain = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default);
  rustPlatform = pkgs.makeRustPlatform { cargo = toolchain; rustc = toolchain; };
in rustPlatform.buildRustPackage rec {
  pname = "BashAgent";
  version = "0.1.0";

  nativeBuildInputs = with pkgs; [ protobuf openssl.dev pkg-config ];
  PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";

  src = ./.;
  cargoLock.lockFile = ./Cargo.lock;

  shellHook = ''PS1="\[\e[1;32m\]\u \W> \[\e[0m\]"'';
}