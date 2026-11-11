#!/bin/sh

TARGET_FILE="src/ui/mod.rs"

if [ ! -f "$TARGET_FILE" ]; then
    zenity --error --text="Erro: Ficheiro $TARGET_FILE não encontrado."
    exit 1
fi

# 1. Pedir a linha original colada pelo utilizador
linha_original=$(zenity --entry --title="Automatizar Tradução" --text="Cole a linha original (ex: let btn = ...):")
[ -z "$linha_original" ] && exit 0

# 2. Processar a linha automaticamente: 
# Substitui o texto entre aspas por tr!("texto")
linha_transformada=$(echo "$linha_original" | sed 's/"\([^"]*\)"/tr!("\1")/g')

# 3. Criar o ficheiro temporário para atualizar o ficheiro principal via awk
TEMP_AWK="/tmp/transform_$$.awk"
cat << 'EOF' > "$TEMP_AWK"
BEGIN {
    orig = ENVIRON["ORIG"]
    dest = ENVIRON["DEST"]
}
{
    if (index($0, orig)) {
        match($0, /^ */)
        indent = substr($0, 1, RLENGTH)
        print indent dest
        next
    }
    print
}
EOF

ORIG="$linha_original" DEST="$linha_transformada" awk -f "$TEMP_AWK" "$TARGET_FILE" > /tmp/mod.rs.new
rm -f "$TEMP_AWK"

# 4. Verificar se o ficheiro foi alterado (se a linha original foi encontrada)
if ! cmp -s "$TARGET_FILE" /tmp/mod.rs.new; then
    # Mostra a linha original e o resultado exato já limpo na caixa de confirmação
    zenity --question --title="Confirmar Alteração" \
        --text="Linha Original:\n$linha_original\n\nResultado Transformado:\n$linha_transformada\n\nDeseja aplicar esta alteração no ficheiro?" \
        --width=700 --height=350

    if [ $? -eq 0 ]; then
        mv /tmp/mod.rs.new "$TARGET_FILE"
        zenity --info --text="Alteração aplicada com sucesso!"
    else
        rm -f /tmp/mod.rs.new
        zenity --notification --text="Alteração rejeitada."
    fi
else
    zenity --warning --text="Aviso: A linha original exata não foi encontrada no ficheiro."
    rm -f /tmp/mod.rs.new
fi