#!/usr/bin/env python3
"""Mantém as traduções em dia. Domínio público.

Uso (na raiz do projeto):
    python3 tools/extract_po.py

O que faz:
  1. Procura no código (src/**/*.rs) todas as frases dentro de  tr("...")  e  trf("...").
  2. Escreve po/bsdconqueror.pot, o "molde" com todas as frases (para começar um novo idioma:
     copia-o para po/xx.po e preenche os msgstr).
  3. Para cada po/*.po diz o que falta traduzir e o que já não existe no código.

Só precisa de Python 3 (sem bibliotecas extra).
"""
import glob
import os
import re

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PO_DIR = os.path.join(ROOT, "po")

# tr("..."), trf("...") ou n_("...") -> apanha o texto entre aspas (com escapes do Rust: \n, \", \\)
CALL = re.compile(r'\b(?:trf?|n_)\(\s*"((?:[^"\\]|\\.)*)"')

HEADER = (
    'msgid ""\n'
    'msgstr ""\n'
    '"Project-Id-Version: BSDConqueror\\n"\n'
    '"MIME-Version: 1.0\\n"\n'
    '"Content-Type: text/plain; charset=UTF-8\\n"\n'
    '"Content-Transfer-Encoding: 8bit\\n"\n'
)


def extract():
    """Devolve {frase: [ficheiro:linha, ...]} pela ordem em que aparecem."""
    found = {}
    for path in sorted(glob.glob(os.path.join(ROOT, "src", "**", "*.rs"), recursive=True)):
        if os.path.basename(path) == "i18n.rs":
            continue  # o próprio mecanismo não tem frases para traduzir
        with open(path, encoding="utf-8") as fh:
            text = fh.read()
        for m in CALL.finditer(text):
            line = text.count("\n", 0, m.start()) + 1
            found.setdefault(m.group(1), []).append(f"{os.path.relpath(path, ROOT)}:{line}")
    return found


def write_pot(found):
    os.makedirs(PO_DIR, exist_ok=True)
    pot = os.path.join(PO_DIR, "bsdconqueror.pot")
    with open(pot, "w", encoding="utf-8") as fh:
        fh.write("# Molde de traduções do BSDConqueror (gerado por tools/extract_po.py).\n")
        fh.write(HEADER)
        for msgid, refs in found.items():
            fh.write("\n")
            for r in refs:
                fh.write(f"#: {r}\n")
            fh.write(f'msgid "{msgid}"\nmsgstr ""\n')
    return pot


def read_po(path):
    """Lê um .po e devolve {frase: tradução}, com os textos tal como estão escritos."""
    entries = {}
    msgid = msgstr = None
    cur = None

    def flush():
        nonlocal msgid, msgstr
        if msgid is not None:
            entries[msgid] = msgstr or ""
        msgid = msgstr = None

    with open(path, encoding="utf-8") as fh:
        for raw in fh:
            line = raw.strip()
            if not line:
                flush()
                cur = None
            elif line.startswith("#"):
                continue
            elif line.startswith("msgid "):
                flush()
                msgid, msgstr, cur = line[6:].strip()[1:-1], "", "id"
            elif line.startswith("msgstr "):
                msgstr, cur = line[7:].strip()[1:-1], "str"
            elif line.startswith('"') and cur:
                piece = line[1:-1]
                if cur == "id":
                    msgid += piece
                else:
                    msgstr += piece
    flush()
    entries.pop("", None)  # cabeçalho
    return entries


def main():
    found = extract()
    pot = write_pot(found)
    print(f"{len(found)} frases no código -> {os.path.relpath(pot, ROOT)}")
    for po in sorted(glob.glob(os.path.join(PO_DIR, "*.po"))):
        entries = read_po(po)
        missing = [m for m in found if not entries.get(m)]
        obsolete = [m for m in entries if m not in found]
        name = os.path.relpath(po, ROOT)
        print(f"\n{name}: {len(found) - len(missing)}/{len(found)} traduzidas")
        if missing:
            print("  Falta traduzir:")
            for m in missing:
                print(f'    msgid "{m}"')
        if obsolete:
            print("  Já não existem no código (podes apagar):")
            for m in obsolete:
                print(f'    msgid "{m}"')


if __name__ == "__main__":
    main()
