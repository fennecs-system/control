# Optional text inference tool. No weights or inference runtime enter Control.
{ pkgs }:
pkgs.dockerTools.buildLayeredImage {
  name = "control-inference";
  tag = "nix";
  contents = [ pkgs.llama-cpp ];
  extraCommands = ''
    mkdir -p tmp models
    chmod 1777 tmp
  '';
  config = {
    User = "65534:65534";
    Env = [ "HOME=/tmp" ];
    Entrypoint = [ "${pkgs.llama-cpp}/bin/llama-server" ];
    Cmd = [ "--host" "0.0.0.0" "--port" "8081" "--no-webui" ];
    ExposedPorts = { "8081/tcp" = { }; };
  };
}
