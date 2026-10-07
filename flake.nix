{
  description = "Pluto development environment";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
    in {
      devShells.${system}.default = pkgs.mkShell {
        nativeBuildInputs = with pkgs; [
          clang
          llvmPackages.lld
          pkg-config
          wayland-scanner
        ];

        buildInputs = with pkgs; [
          foot
          wayland
          wayland-protocols
          libxkbcommon
          libinput
          seatd
          libdrm
          libgbm
          libglvnd
          mesa
          udev
        ];

        shellHook = ''
          export CC=clang
          export CXX=clang++
          export WAYLAND_DISPLAY=wayland-1

          export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath [
            pkgs.wayland
            pkgs.libxkbcommon
            pkgs.libinput
            pkgs.seatd
            pkgs.libdrm
            pkgs.libgbm
            pkgs.libglvnd
            pkgs.mesa
            pkgs.udev
          ]}:$LD_LIBRARY_PATH"
        '';
      };
    };
}

