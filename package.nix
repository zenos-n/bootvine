{ lib, rustPlatform, pkg-config, dbus, systemd }:
rustPlatform.buildRustPackage {
  pname = "bootvine";
  version = "0.1.3-zenos1";
  src = lib.cleanSource ./.;
  cargoLock.lockFile = ./Cargo.lock;
  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ dbus systemd ];
  cargoBuildFlags = [ "-p" "bootvine" ];
  cargoTestFlags = [ "-p" "bootvine" ];
  postInstall = ''
    install -Dm644 nix/plymouth-bootvine.patch $out/share/bootvine/plymouth-bootvine.patch
    for unit in bootvine.service bootvine-shutdown.service; do
      install -Dm644 bootvine/system-files/systemd/$unit $out/lib/systemd/system/$unit
      substituteInPlace $out/lib/systemd/system/$unit --replace-fail /usr/bin/bootvine $out/bin/bootvine
    done
  '';
  meta = { description = "Live boot tree with Plymouth Escape handoff"; mainProgram = "bootvine"; license = lib.licenses.gpl3Plus; platforms = lib.platforms.linux; };
}
