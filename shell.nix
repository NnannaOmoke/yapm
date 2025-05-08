{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  buildInputs = with pkgs; [
    libseccomp
    #need this shit to manage libseccomp  
    pkg-config
    
  ];
  
  shellHook = ''
    echo "Nix shell with libseccomp is ready!"
    echo "libseccomp version: $(pkg-config --modversion libseccomp)"
  '';
}
