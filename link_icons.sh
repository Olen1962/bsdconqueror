#!/bin/sh
# Procura ícones adequados ao BSDConqueror no tema hicolor (e, se faltarem,
# noutros temas em /usr/local/share/icons) e cria-os em src/icons.
#
# Uso:
#   ./link_icons.sh [-c] [-f] [destino]
#   ./link_icons.sh -l              # só lista os nomes de ícones de ações disponíveis
#
#   -c   copia os ficheiros (cp -L) em vez de criar links simbólicos
#        (recomendado se o projeto vai para o git)
#   -f   substitui ficheiros que já existam (por omissão NÃO mexe neles)
#
# Ficheiros: <papel>-<tamanho>.<ext>   (ex.: quit-32x32.png, add-scalable.svg)

ICON_ROOT="/usr/local/share/icons"
DEST_DEFAULT="/home/olen/1TB/git/bsdconqueror/src/icons"
SIZES="16x16 22x22 24x24 32x32 48x48 scalable"
CONTEXTS="actions apps status places categories devices emblems mimetypes"
MODE=link
FORCE=0
LIST=0

while getopts "lcf" opt; do
    case $opt in
        l) LIST=1 ;;
        c) MODE=copy ;;
        f) FORCE=1 ;;
        *) echo "Uso: $0 [-c] [-f] [-l] [destino]"; exit 1 ;;
    esac
done
shift $((OPTIND - 1))

# hicolor primeiro; depois os restantes temas instalados
THEMES="$ICON_ROOT/hicolor"
for t in "$ICON_ROOT"/*/; do
    t="${t%/}"
    [ "$t" = "$ICON_ROOT/hicolor" ] && continue
    [ -d "$t" ] && THEMES="$THEMES $t"
done

if [ "$LIST" = 1 ]; then
    for t in $THEMES; do
        echo "== $t"
        for s in $SIZES; do
            ls "$t/$s/actions" 2>/dev/null
        done | sed 's/\.[a-z]*$//' | sort -u | tr '\n' ' '
        echo
    done
    exit 0
fi

DEST="${1:-$DEST_DEFAULT}"
mkdir -p "$DEST" || { echo "Não consegui criar $DEST"; exit 1; }

# find_icon <nome> <tamanho>  -> imprime o caminho do primeiro ficheiro encontrado
find_icon() {
    for theme in $THEMES; do
        for ctx in $CONTEXTS; do
            for ext in png svg; do
                f="$theme/$2/$ctx/$1.$ext"
                if [ -e "$f" ]; then
                    echo "$f"
                    return 0
                fi
            done
        done
    done
    return 1
}

# link_role <papel> <nome1> <nome2> ...   (usa o primeiro nome que existir)
link_role() {
    role="$1"; shift
    for name in "$@"; do
        found=0; made=0; kept=0
        for size in $SIZES; do
            src="$(find_icon "$name" "$size")" || continue
            found=1
            ext="${src##*.}"
            dst="$DEST/$role-$size.$ext"
            if { [ -e "$dst" ] || [ -L "$dst" ]; } && [ "$FORCE" = 0 ]; then
                kept=$((kept + 1))
                continue
            fi
            if [ "$MODE" = copy ]; then
                rm -f "$dst"
                cp -L "$src" "$dst"
            else
                ln -sf "$src" "$dst"
            fi
            made=$((made + 1))
        done
        if [ "$found" = 1 ]; then
            echo "OK     $role  <-  $name   (novos: $made, já existiam: $kept)"
            return 0
        fi
    done
    echo "FALTA  $role  (tentei: $*)"
    return 1
}

link_role search      edit-find system-search find
link_role refresh     view-refresh reload
link_role reset       edit-undo view-restore edit-clear
link_role select_all  edit-select-all select-all
link_role clear       edit-clear-all edit-clear edit-delete
link_role lock        changes-prevent object-locked emblem-readonly
link_role unlock      changes-allow object-unlocked
link_role app         utilities-system-monitor applications-system preferences-system
link_role quit        application-exit window-close system-log-out
link_role add         list-add list-add-symbolic go-next go-down
link_role remove      list-remove edit-delete user-trash window-close

echo
echo "Ficheiros em $DEST:"
ls -l "$DEST" | grep -E "quit-|add-"
