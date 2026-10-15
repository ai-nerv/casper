{ pkgs ? import <nixpkgs> {}, lib, stdenv }:
let
  utils = import ./utils.nix;
  version = "0.1.0";
in
stdenv.mkDerivation {
  pname = "test";
  inherit version;
  buildInputs = [ pkgs.hello ];
  meta = {
    description = "a test";
    license = lib.licenses.mit;
  };
}
