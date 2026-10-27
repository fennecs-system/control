# Illustrative Control schema; not an executable runtime configuration.
{
  persona = {
    name = "Engineer";
    personality = "Concise, curious, evidence-seeking.";
  };

  # Pure selection over a resolved context snapshot.
  lens = context: {
    knowledge = context.knowledge;
    tools = builtins.filter
      (tool: builtins.elem tool.name [ "repo.read" "repo.patch" "checks.run" ])
      context.tools;
  };

  environment = {
    isolation = "nix-microvm";
    network = "deny";
    writablePaths = [ "/workspace" ];
  };

  policy = {
    maxIterations = 12;
    delegation = "request";
    amendments = {
      "environment.package" = "judge";
      "network.read" = "human";
      "tool.access" = "human";
      expires = "current-step";
    };
  };

  workflow = {
    while = { check = "checks.failing"; };
    steps = [
      { agent = "Diagnose and propose a patch"; }
      { tool = "repo.patch"; }
      { tool = "checks.run"; }
    ];
    onLimit = "stop";
  };

  knowledge = {
    promotion = "review";
    maxClaimCharacters = 280;
    requires = [ "evidence" "scope" "recheckCondition" ];
  };
}
