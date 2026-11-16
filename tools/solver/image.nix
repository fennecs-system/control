# Optional optimisation/constraint solver tool. Control runs one solver per call
# in a fresh offline container and sends the model on stdin; nothing is mounted.
# Adding a solver: add its package here and one entry to `SOLVERS` in
# src/solver.rs (see docs/SOLVERS.md). The label below is the provenance Control
# reports with every result and the list of solvers this image provides.
{ pkgs }:
let
  inherit (pkgs) lib;
  # Copies the named files (shell globs relative to `package`) into a new store
  # path, points ELF library search paths at the copies, and fails if the copy
  # still refers to the original package. This keeps cached binaries
  # while dropping optional parts of a package's closure that a CLI never loads.
  slim = { name, package, files }:
    pkgs.runCommand "control-${name}" { nativeBuildInputs = [ pkgs.patchelf ]; } ''
      cd ${package}
      for file in ${files}; do
        mkdir -p "$out/$(dirname "$file")"
        cp -RP "$file" "$out/$file"
      done
      chmod -R u+w $out
      find $out -type f | while read -r file; do
        rpath=$(patchelf --print-rpath "$file" 2>/dev/null) || continue
        patchelf --set-rpath "$(printf %s "$rpath" | sed "s|${package}/lib|$out/lib|g")" "$file"
      done
      if grep -rl ${package} $out; then
        echo "control-${name} still refers to ${package}" >&2
        exit 1
      fi
    '';
  # or-tools also installs Python bindings (pandas, numpy, protobuf) that the
  # FlatZinc executable never loads.
  orTools = pkgs.or-tools;
  cpSat = slim {
    name = "cp-sat-${orTools.version}";
    package = orTools;
    files = "bin/fzn-cp-sat lib/libortools*.so* share/minizinc/cp-sat share/minizinc/solvers/cp-sat.msc";
  };
  # MiniZinc compiles .mzn models to FlatZinc. Its built-in Gecode backend
  # links Gecode's Qt visualiser, which would add a desktop GUI stack to an
  # offline tool; CP-SAT is the backend here, so build MiniZinc without Gecode.
  minizinc = pkgs.minizinc.overrideAttrs (old: {
    buildInputs = lib.remove pkgs.gecode old.buildInputs;
    postInstall = "";
  });
  # Registry of packaged solvers. `frontend` records a modelling layer used to
  # run the solver; versions become the image's provenance label.
  solvers = {
    highs = { package = pkgs.highs; };
    z3 = { package = pkgs.z3; };
    cp-sat = { package = orTools; installed = cpSat; frontend = minizinc; };
  };
  provenance = builtins.toJSON (lib.mapAttrs (_: solver: {
    package = solver.package.pname;
    version = solver.package.version;
  } // lib.optionalAttrs (solver ? frontend) {
    frontend = "${solver.frontend.pname} ${solver.frontend.version}";
  }) solvers);
  tools = pkgs.buildEnv {
    name = "control-solver-tools";
    paths = [ pkgs.bash pkgs.coreutils ]
      ++ lib.concatMap (solver: [ (solver.installed or solver.package) ] ++ lib.optional (solver ? frontend) solver.frontend)
        (lib.attrValues solvers);
    pathsToLink = [ "/bin" ];
  };
in
pkgs.dockerTools.buildLayeredImage {
  name = "control-solver";
  tag = "nix";
  contents = [ tools (pkgs.writeTextDir "etc/control-solvers.json" provenance) ];
  extraCommands = ''
    mkdir -p tmp
    chmod 1777 tmp
  '';
  config = {
    User = "65534:65534";
    WorkingDir = "/tmp";
    Env = [
      "PATH=/bin"
      "HOME=/tmp"
      "MZN_SOLVER_PATH=${cpSat}/share/minizinc/solvers"
      "MZN_STDLIB_DIR=${minizinc}/share/minizinc"
    ];
    Labels = {
      "dev.control.environment" = "solver-v1";
      "dev.control.solvers" = provenance;
    };
    Cmd = [ "highs" "--version" ];
  };
}
