{
  description = "A native, terminal-friendly GDB frontend";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forEachSystem = nixpkgs.lib.genAttrs systems;
      packagesFor = system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };

          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = toolchain;
            rustc = toolchain;
          };

          fgdb = pkgs.callPackage ./packaging/nix/package.nix { inherit rustPlatform; };
        in {
          inherit fgdb;
          default = fgdb;
          fgdb-no-ebpf = fgdb.override { ebpfSupport = false; };
        };
    in {
      packages = forEachSystem packagesFor;

      checks = forEachSystem (system: {
        inherit (self.packages.${system}) fgdb fgdb-no-ebpf;
      });
    };
}
