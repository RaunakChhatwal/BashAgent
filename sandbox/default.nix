{ nixpkgs, nixos-generators, system, bash-agent, ... }:
let image_config = {
  virtualisation.diskSize = 64*1024;
  nix.registry.nixpkgs.flake = nixpkgs;
};
in nixos-generators.nixosGenerate {
  inherit system;
  specialArgs.bash-agent = bash-agent;
  modules = [ image_config ./configuration.nix ];
  format = "qcow";
}
