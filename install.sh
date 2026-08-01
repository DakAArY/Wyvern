REPOSITORY="DakAArY/Wyvern"
ASSET_NAME="wyvern-linux-x86_64"
COMMAND_NAME="wyvern"
INSTALL_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DOWNLOAD_URL="https://github.com/${REPOSITORY}/releases/latest/download/${ASSET_NAME}"

if [[ "$(uname -s)" != "Linux" ]]; then
    echo "Error: este instalador solo es compatible con Linux."
    exit 1
fi

case "$(uname -m)" in
    x86_64|amd64)
        ;;
    *)
        echo "Error: esta versión requiere una arquitectura x86_64."
        echo "Arquitectura detectada: $(uname -m)"
        exit 1
        ;;
esac

if command -v curl >/dev/null 2>&1; then
    DOWNLOADER="curl"
elif command -v wget >/dev/null 2>&1; then
    DOWNLOADER="wget"
else
    echo "Error: necesitas instalar curl o wget."
    exit 1
fi

mkdir -p "$INSTALL_DIR"

TEMPORARY_FILE="$(mktemp)"
trap 'rm -f "$TEMPORARY_FILE"' EXIT

echo "Descargando Wyvern..."

if [[ "$DOWNLOADER" == "curl" ]]; then
    curl --fail --location --progress-bar \
        "$DOWNLOAD_URL" \
        --output "$TEMPORARY_FILE"
else
    wget --show-progress \
        "$DOWNLOAD_URL" \
        --output-document="$TEMPORARY_FILE"
fi

install -m 0755 "$TEMPORARY_FILE" "$INSTALL_DIR/$COMMAND_NAME"

echo
echo "Wyvern se instaló correctamente en:"
echo "$INSTALL_DIR/$COMMAND_NAME"

case ":${PATH:-}:" in
    *":$INSTALL_DIR:"*)
        echo
        echo "Ya puedes ejecutarlo con:"
        echo "wyvern"
        ;;
    *)
        echo
        echo "La carpeta no está en tu PATH."
        echo "Añádela con:"
        echo
        echo "export PATH=\"$INSTALL_DIR:\$PATH\""
        echo
        echo "Después reinicia la terminal o ejecuta ese comando."
        ;;
esac