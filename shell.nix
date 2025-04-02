# shell.nix
{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  buildInputs = with pkgs; [
    libseccomp
    
    pkg-config
    
  ];
  
  # Set up environment variables for libseccomp
  shellHook = ''
    
    echo "Nix shell with libseccomp is ready!"
    echo "libseccomp version: $(pkg-config --modversion libseccomp)"
  '';
}
