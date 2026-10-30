# Offline Firefox tool, assembled from Nix closures without a distribution base.
{ pkgs }:
let
  fontsConf = pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; };
  fonts = pkgs.runCommand "control-browser-fonts" { } ''
    mkdir -p $out/etc/fonts
    cat > $out/etc/fonts/fonts.conf <<EOF
    <?xml version="1.0"?>
    <!DOCTYPE fontconfig SYSTEM "fonts.dtd">
    <fontconfig>
      <include>${fontsConf}</include>
      <alias binding="strong"><family>monospace</family><prefer><family>DejaVu Sans Mono</family></prefer></alias>
    </fontconfig>
    EOF
  '';
  runner = pkgs.runCommand "control-browser" { nativeBuildInputs = [ pkgs.rustc pkgs.stdenv.cc ]; } ''
    mkdir -p $out/bin src/browser
    cp ${./main.rs} src/browser/main.rs
    cp ${../platform-flags.rs} src/platform-flags.rs
    rustc --edition=2024 -O src/browser/main.rs -o $out/bin/control-browser
  '';
in pkgs.dockerTools.buildLayeredImage {
  name = "control-browser";
  tag = "nix";
  contents = [ runner fonts pkgs.firefox pkgs.dejavu_fonts pkgs.fontconfig ];
  extraCommands = ''
    mkdir -p tmp input
    chmod 1777 tmp
  '';
  config = {
    Env = [ "PATH=/bin" "HOME=/tmp" "MOZ_HEADLESS=1" "FONTCONFIG_FILE=/etc/fonts/fonts.conf" ];
    Entrypoint = [ "${runner}/bin/control-browser" ];
  };
}
