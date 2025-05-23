{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  buildInputs = with pkgs; [
    libseccomp
    #need this shit to manage libseccomp  
    pkg-config
    bc
    
  ];
  
  shellHook = ''
    echo "Nix shell with libseccomp is ready!"
    echo "libseccomp version: $(pkg-config --modversion libseccomp)"
  '';
}
