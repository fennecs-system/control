# Optional text inference tool. No weights or inference runtime enter Control.
# `acceleration` selects the pinned llama.cpp build: "cpu" (portable default),
# "vulkan" (pass /dev/dri) or "cuda" (unfree; needs the NVIDIA container toolkit).
{ pkgs, acceleration ? "cpu" }:
let
  llama = {
    cpu = pkgs.llama-cpp;
    vulkan = pkgs.llama-cpp.override { vulkanSupport = true; };
    cuda = pkgs.llama-cpp.override { cudaSupport = true; };
  }.${acceleration};
  suffix = if acceleration == "cpu" then "" else "-${acceleration}";
in
pkgs.dockerTools.buildLayeredImage {
  name = "control-inference";
  tag = "nix${suffix}";
  # Vulkan needs the Mesa ICDs inside the image; the host passes /dev/dri.
  contents = [ llama ] ++ pkgs.lib.optional (acceleration == "vulkan") pkgs.mesa;
  extraCommands = ''
    mkdir -p tmp models
    chmod 1777 tmp
  '';
  config = {
    User = "65534:65534";
    Env = [ "HOME=/tmp" ] ++ pkgs.lib.optional (acceleration == "vulkan") "XDG_DATA_DIRS=/share";
    Entrypoint = [ "${llama}/bin/llama-server" ];
    Cmd = [ "--host" "0.0.0.0" "--port" "8081" "--no-webui" ];
    ExposedPorts = { "8081/tcp" = { }; };
  };
}
