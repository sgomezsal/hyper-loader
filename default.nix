{ pkgs }:
pkgs.rustPlatform.buildRustPackage {
  pname = "hyper-loader";
  version = "0.1.0";

  src = ./.;

  cargoLock.lockFile = ./Cargo.lock;

  nativeBuildInputs = with pkgs; [
    pkg-config
  ];

  buildInputs = with pkgs; [
    wayland
    libxkbcommon
  ];

  meta = {
    description = "Native wlr-layer-shell overlay for long-running commands (e.g. nixos-rebuild switch)";
    mainProgram = "hyper-loader";
    license = pkgs.lib.licenses.mit;
  };
}
