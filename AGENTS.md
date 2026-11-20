AGENTS.md — BSDConqueror
========================

Visão geral
-----------
Pesquisador e editor de memória nativo para FreeBSD, escrito em Rust (egui/eframe).
Usa exclusivamente chamadas do sistema base (ps, procstat, ptrace(2) / PT_ATTACH / PT_IO),
sem dependências externas como scanmem.

Arquitetura do projeto
---------------------
- src/main.rs: ponto de entrada, inicializa eframe/egui.
- src/core/scanner.rs: lógica central de leitura/escrita de memória (ptrace, procstat),
  pesquisa byte-a-byte, filtragem de candidatos e locks periódicos.
- src/ui/mod.rs: interface gráfica em egui/eframe (listas Found / lista de baixo,
  atalhos, preferências).
- src/icons/: ícones PNG embutidos no binário (svg/ com originais).
- tools/make_icons.py: script Python 3 + Pillow para regenerar ícones.
- src/i18n.rs: traduções via ficheiros .po embebidos no executável (tr(), trf(), n_()).
- po/: ficheiros de tradução .po (pt, es, de, fr, it, pl; o inglês são as próprias frases do código).

Thread separada
---------------
O scanner roda em thread própria e comunica com a UI por canais (mpsc). A interface
nunca bloqueia durante uma pesquisa longa.

Comportamento de ptrace
-----------------------
- PT_ATTACH pausa o processo alvo durante leitura/escrita (PT_IO) e desanexa logo a seguir.
- Manter as secções anexadas o mais curto possível.
- **A primeira pesquisa demora vários segundos enquanto o processo fica congelado.**
  Pesquisas seguintes e a operação de sync são mais rápidos — o attach dura só o tempo
  de ler/escrever os endereços candidatos.

Compilação, testes e execução
-----------------------------
Sempre usar --release para executar: a pesquisa byte-a-byte em modo debug é excessivamente lenta.

```sh
cargo check                 # verificação rápida
cargo test                  # testes unitários (não precisam de processo alvo)
cargo test pattern          # um grupo de testes; idem para i18n
cargo build --release       # compila versão otimizada
./target/release/bsdconqueror
```

A primeira compilação pode demorar vários minutos (egui/eframe). Os testes unitários
correm em modo debug sem problema; não fazem ptrace.

Permissões
----------
- Processos do mesmo utilizador: não precisa root se `security.bsd.unprivileged_proc_debug=1`
  (valor padrão no FreeBSD).
- Processos de outros utilizadores (incluindo root) ou sysctl valendo 0: executar como root.
- Se aparecer "PT_ATTACH falhou ... Operation not permitted", falta uma destas permissões.

Tipos suportados e limites
-------------------------
- Inteiros de 1, 2, 4 e 8 bytes (pesquisa desalinhada byte-a-byte no modo exato).
- Float (4 bytes, f32) e double (8 bytes, f64), IEEE 754. São pesquisados pelo mesmo
  mecanismo de varredura byte-a-byte, por **bytes exatos** (sem tolerância).
  O modo Auto inclui float e double juntamente com os tipos inteiros.
- O valor pesquisado/escrito pode ser inteiro (decimal, 0x, 0b, 0o) ou decimal com ponto
  ou vírgula (`1.5`, `1,5`, `1e3`). Um decimal ignora os tipos inteiros no modo Auto;
  `100.0` conta como inteiro.
- **Valor desconhecido**: a primeira pesquisa não leva valor e guarda um instantâneo;
  as seguintes filtram por `Increased`, `Decreased`, `Changed`, `Unchanged` ou `Equal`.
  Neste modo escolhe-se **um só tipo** (1, 2, 4, 8, float ou double) e a varredura é
  alinhada ao tamanho desse tipo.
- Limite de 16 milhões de candidatos na pesquisa exata e 8 milhões no modo desconhecido.
  O histórico de Undo do modo desconhecido guarda no máximo 1 milhão de candidatos.
- Apenas regiões de memória com permissão de leitura/escrita (sem devices nem guard pages).
- Máximo de 20 endereços mostrados na lista Found; valores iguais com larguras diferentes
  mostram-se apenas a mais estreita.
- Não há pesquisa de ponteiros.

Memória e varredura
-------------------
- No modo exato a varredura percorre todas as regiões RW do processo, com passo de 1 byte.
- Necessária para encontrar valores desalinhados (típico de jogos DOS em emuladores).
- No modo de valor desconhecido a varredura avança pelo tamanho do tipo e guarda um `Num`
  por endereço em `MemoryScanner::unknown_values`, em paralelo com `candidates`.
- Resultados podem ser truncados se ultrapassarem os limites acima.
- Lock (cadeado) escreve o valor continuamente a cada 0,3 s (ou 1 s sem locks) para mantê-lo fixo.

Configuração
------------
Preferências guardadas em formato INI simples em
`~/.config/bsdconqueror/config.ini` (ou `$XDG_CONFIG_HOME/bsdconqueror/`).
É um ficheiro de texto simples. Para repor valores por omissão, apaga-o.
As chaves `unknown_width` e `search_mode` guardam o último tipo/comparação do modo
desconhecido (`search_mode` usa os códigos `increased`, `decreased`, `changed`,
`unchanged`, `equal`). As chaves `lang` e `theme` guardam o idioma e o tema
(`light`, `dark` ou `system`).

Atalhos principais
------------------
- `Enter` na caixa Value: pesquisar
- `F2`: editar valor da linha selecionada
- `Ctrl+A`: selecionar tudo (lista de baixo)
- `Delete`: remover linhas selecionadas
- `Esc`: desselecionar
- `Ctrl` + roda do rato: zoom
- Clique duplo em Value/Description: editar

Desfazer (undo)
---------------
- O histórico guarda até 5 listas anteriores de candidatos (máx. 2M cada; 1M no modo
  desconhecido, porque cada candidato guarda também um `Num`).
- Cada snapshot guarda `candidates` e, no modo desconhecido, `unknown_values`.
- `Undo` repõe a lista anterior; no modo exato os valores mostrados são lidos da memória,
  no modo desconhecido vêm do snapshot restaurado.

Convenções de largura (scanner)
-------------------------------
- Código de largura: `1`, `2`, `4`, `8` = inteiros (tamanho em bytes); `5` = float32
  (4 bytes); `6` = double (8 bytes). O código NÃO é o tamanho em memória: usar
  `byte_len()` para isso.
- Os valores são do tipo `Num` (`Int(i64)`, `F32(f32)`, `F64(f64)`), definido em
  `src/core/scanner.rs`. `decode()` devolve `Num`; `pattern()` converte `Num` em bytes.
  `parse_num()` (em `src/ui/mod.rs`) converte o texto escrito pelo utilizador.
- `Comparison` (`Increased`, `Decreased`, `Changed`, `Unchanged`, `Equal`) e
  `matches_comparison()` pertencem ao scanner. O modo exato continua a comparar **bytes**;
  o modo desconhecido compara **valores** (`Num`) e nunca deve usar igualdade de bytes.
- Cada candidato é um `u64`: endereço nos 61 bits baixos e código de largura nos 3 bits
  altos (`TAG_SHIFT = 61`, `pack()`/`unpack()`). Não alargar o endereço nem a etiqueta
  sem rever estas duas funções.
- `ALL_WIDTHS` = `[1, 2, 4, 5, 6, 8]`, sem repetições.
- `SCAN_NAMES`/`scan_widths()` em `src/ui/mod.rs`: índice 7 = Auto; índice 8 = valor
  desconhecido, que usa `unknown_width` em vez de `scan_widths()`. Não usar
  `SCAN_NAMES.len() - 1` para detetar o Auto.

Tarefas pendentes
-----------------

### Pesquisa de ponteiros
Não implementada nem prioritária. Para emuladores (DOSBox, ScummVM) e jogos retro no
FreeBSD, o espaço de memória emulado opera em blocos contíguos com endereçamento estático
e linear (sem ASLR nem ponteiros multinível complexos). O foco do projeto mantém-se na
varredura byte-a-byte desalinhada, no modo de valor desconhecido e na rapidez de
operações nativas com ptrace. Não confundir com o modo de valor desconhecido, que já
está implementado em `perform_unknown_first_scan()`/`perform_unknown_next_scan()`.

### Testes de integração
Os testes unitários cobrem `pattern()`, `decode()`, `pack()`/`unpack()`, `byte_len()` e
as comparações de `Num`. Não há testes com `PT_ATTACH` real: a leitura/escrita de memória
continua a exigir verificação manual com um processo alvo.

### Traduções
Todas as frases atuais estão traduzidas nos seis `.po` (`python3 tools/extract_po.py`
deve reportar tudo traduzido). Ao acrescentar ou alterar frases em `tr()`/`trf()`/`n_()`:
1. escrever a frase em inglês no código;
2. correr `python3 tools/extract_po.py`;
3. preencher os `msgstr` nos seis ficheiros;
4. garantir que cada `{}` existe na tradução (teste
   `traducoes_tem_os_mesmos_marcadores`, em `src/i18n.rs`).

Licença
-------
Domínio público (Unlicense) para todo o código original.