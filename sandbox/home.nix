{ config, pkgs, ... }: {
  home.username = "claude";
  home.homeDirectory = "/home/claude";

  home.stateVersion = "25.05";	# do not change this

  home.packages = with pkgs; [
    file
    git
    openssl
    tree
    zip unzip zlib
    fd
    ripgrep
    (python3.withPackages (python-pkgs: with python-pkgs; [
      numpy
      ipython
      matplotlib
      requests
      openai
      jupyter
      json5
    ]))
  ];

  home.file = {
    ".bashrc".text = ''PS1="\u \W> "'';
    ".profile".text = ". ~/.bashrc";
  };

  programs.home-manager.enable = true;
}
