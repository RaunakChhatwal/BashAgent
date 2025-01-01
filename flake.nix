{
  description = "BashAgent";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nixos-generators = {
      url = "github:nix-community/nixos-generators";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, flake-utils, rust-overlay, nixos-generators, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        bash-agent = (import ./default.nix) { inherit pkgs; };
      in {
        packages.default = bash-agent;
        packages.image = (import ./sandbox/default.nix) {
          inherit nixpkgs nixos-generators system bash-agent;
        };
        devShells.default = bash-agent;
      }
    );
}
