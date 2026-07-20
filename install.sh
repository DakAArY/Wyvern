set -euo pipefail

REPO="https://github.com/DakAArY/Wyvern"
BIN_NAME="Wyvern"

if ! command -v cargo &> /dev/null; then
    echo "Error: no se encontró 'cargo' en el PATH." >&2
    echo "Instala Rust primero desde https://rustup.rs y vuelve a correr este script." >&2
    exit 1
fi

echo "==> Instalando/actualizando ${BIN_NAME} desde ${REPO}"
cargo install --git "${REPO}" --bin "${BIN_NAME}" --force

CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"

if [[ ":${PATH}:" == *":${CARGO_BIN}:"* ]]; then
    echo "==> ${CARGO_BIN} ya está en tu PATH."
else
    echo "==> ${CARGO_BIN} no está en tu PATH todavía. Lo agrego a tu configuración de shell."

    SHELL_NAME="$(basename "${SHELL:-}")"
    case "${SHELL_NAME}" in
        fish)
            SHELL_RC="$HOME/.config/fish/config.fish"
            LINE="set -gx PATH \$PATH ${CARGO_BIN}"
            ;;
        zsh)
            SHELL_RC="$HOME/.zshrc"
            LINE="export PATH=\"\$PATH:${CARGO_BIN}\""
            ;;
        bash|*)
            SHELL_RC="$HOME/.bashrc"
            LINE="export PATH=\"\$PATH:${CARGO_BIN}\""
            ;;
    esac

    mkdir -p "$(dirname "${SHELL_RC}")"
    touch "${SHELL_RC}"

    if ! grep -qF "${CARGO_BIN}" "${SHELL_RC}"; then
        {
            echo ""
            echo "# Agregado por install.sh de ${BIN_NAME}"
            echo "${LINE}"
        } >> "${SHELL_RC}"
        echo "==> Se agregó ${CARGO_BIN} a ${SHELL_RC}."
        echo "    Reiniciá la terminal, o corré: source ${SHELL_RC}"
    else
        echo "==> ${SHELL_RC} ya tenia una referencia a ${CARGO_BIN}, no se toco."
    fi
fi

echo "==> Listo."
