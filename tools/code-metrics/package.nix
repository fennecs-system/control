{ pkgs }:
let
  # Upstream v5 is a native Rust executable, not the older Node implementation.
  archives = {
    aarch64-darwin = [ "darwin-arm64" "e119c4c5388118ad348a799c478589f7ad0fa707c0b7c819d6febc94f6f98522" ];
    x86_64-darwin = [ "darwin-x64" "7e5b81a0b30f1d261bf9cd8d8e72be43f0ce55a54a9374f1111123bb29362362" ];
    aarch64-linux = [ "linux-arm64-musl" "f363875348817f8e999ca090266b1265ba29322d6b5d1f6280aa0d136ab1ca9e" ];
    x86_64-linux = [ "linux-x64-musl" "291452d6005e3fa17b531552bf76f87ff85bc4ae31533e0c17a7997f84079868" ];
  };
  archive = archives.${pkgs.stdenv.hostPlatform.system};
  jscpd = pkgs.stdenvNoCC.mkDerivation {
    pname = "jscpd";
    version = "5.4.3";
    src = pkgs.fetchurl {
      url = "https://github.com/kucherenko/jscpd/releases/download/v5.4.3/jscpd-${builtins.elemAt archive 0}.tar.gz";
      sha256 = builtins.elemAt archive 1;
    };
    sourceRoot = ".";
    dontBuild = true;
    dontStrip = true;
    installPhase = ''
      install -Dm755 jscpd $out/bin/jscpd
    '';
  };
in pkgs.rustPlatform.buildRustPackage {
  pname = "control-code-metrics";
  version = "0.1.0";
  src = pkgs.lib.cleanSource ./.;
  cargoLock.lockFile = ./Cargo.lock;
  nativeBuildInputs = [ pkgs.makeWrapper ];
  postInstall = ''
    wrapProgram $out/bin/control-code-metrics \
      --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.tokei pkgs.rust-code-analysis jscpd ]}
  '';
}
