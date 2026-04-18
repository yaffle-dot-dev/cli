{ pkgs
, lib ? pkgs.lib
, src ? ../.
}:

let
  sourceFilter = path: type:
    let
      baseName = baseNameOf path;
    in
    !(
      baseName == ".git" ||
      baseName == "node_modules" ||
      baseName == "dist" ||
      baseName == "result" ||
      baseName == ".DS_Store" ||
      lib.hasSuffix ".log" baseName
    );

  filteredSrc = lib.cleanSourceWith {
    inherit src;
    filter = sourceFilter;
    name = "yaffle-cli-src";
  };

in
pkgs.stdenv.mkDerivation {
  pname = "yaffle-cli";
  version = "0.1.0";

  src = filteredSrc;

  nativeBuildInputs = [ pkgs.bun pkgs.cacert ];

  HOME = "/tmp";

  configurePhase = ''
    runHook preConfigure
    bun install --frozen-lockfile
    runHook postConfigure
  '';

  buildPhase = ''
    runHook preBuild
    bun build src/main.ts --compile --outfile yaffle
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall

    install -Dm755 yaffle $out/bin/yaffle

    cat > $out/bin/yaffle-outputs <<EOF
#!/bin/sh
exec $out/bin/yaffle outputs "$@"
EOF
    chmod +x $out/bin/yaffle-outputs

    runHook postInstall
  '';

  meta = with lib; {
    description = "Yaffle CLI";
    homepage = "https://yaffle.dev";
    license = licenses.mit;
    platforms = platforms.all;
    mainProgram = "yaffle";
  };
}
