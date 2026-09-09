{
  lib,
  rustPlatform,
  pkg-config,
  wrapGAppsHook4,
  glib,
  gtk4,
  gtksourceview5,
  vte-gtk4,
  gdb,
  clang,
  elfutils,
  zlib,
  ebpfSupport ? true,
}:
let
  manifest = builtins.fromTOML (builtins.readFile ../../Cargo.toml);
in
rustPlatform.buildRustPackage {
  pname = if ebpfSupport then "fgdb" else "fgdb-no-ebpf";
  inherit (manifest.package) version;

  src = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.unions [
      ../../Cargo.toml
      ../../Cargo.lock
      ../../build.rs
      ../../src
      ../../examples
      ../../assets
      ../../LICENSE
    ];
  };

  cargoLock.lockFile = ../../Cargo.lock;
  buildNoDefaultFeatures = true;
  buildFeatures = lib.optional ebpfSupport "ebpf";

  # ELF inspection tests read debug sections from their own executable.
  checkType = "debug";

  nativeBuildInputs = [ pkg-config wrapGAppsHook4 glib ];
  buildInputs = [ gtk4 gtksourceview5 vte-gtk4 ]
    ++ lib.optionals ebpfSupport [ elfutils zlib ];

  # The unwrapped compiler does not inject host linker flags into BPF builds.
  env = lib.optionalAttrs ebpfSupport {
    BPF_CLANG = "${clang.cc}/bin/clang";
  };

  preFixup = ''
    gappsWrapperArgs+=(--suffix PATH : ${lib.makeBinPath [ gdb ]})
  '';

  postInstall = ''
    install -Dm644 assets/dev.fgdb.Fgdb.desktop "$out/share/applications/dev.fgdb.Fgdb.desktop"
    install -Dm644 LICENSE "$out/share/licenses/fgdb/LICENSE"
    mkdir -p "$out/share/icons"
    cp -a assets/icons/hicolor "$out/share/icons/"
  '';

  meta = {
    inherit (manifest.package) description;
    homepage = manifest.package.repository;
    license = lib.licenses.mit;
    mainProgram = "fgdb";
    platforms = [ "x86_64-linux" "aarch64-linux" ];
  };
}
