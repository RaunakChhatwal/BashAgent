{
  description = "NixOS configuration.";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
  inputs.bash-agent.url = "github:RaunakChhatwal/BashAgent/main";

  outputs = { nixpkgs, bash-agent, ... }:
  let
    system = "x86_64-linux";
    pkgs = import nixpkgs { inherit system; };
  in {
    nixosConfigurations.nixos = nixpkgs.lib.nixosSystem {
      specialArgs = {
        inherit system;
        bash-agent = bash-agent.packages.${system}.default;
      };
      modules = [ ./nixos/configuration.nix ];
    };
  };
}
