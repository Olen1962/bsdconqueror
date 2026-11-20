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

Compilação e execução
---------------------
Sempre usar --release: a pesquisa byte-a-byte em modo debug é excessivamente lenta.

```sh
cargo build --release    # compila versão otimizada
./target/release/bsdconqueror  # executa
```

Permissões
----------
- Processos do mesmo utilizador: não precisa root se `security.bsd.unprivileged_proc_debug=1`
  (valor padrão no FreeBSD).
- Processos de outros utilizadores (incluindo root) ou sysctl valendo 0: executar como root.
- Se aparecer "PT_ATTACH falhou ... Operation not permitted", falta uma destas permissões.

Tipos suportados e limites
-------------------------
- Inteiros de 1, 2, 4 e 8 bytes (pesquisa desalinhada byte-a-byte).
- Float (4 bytes, f32) e double (8 bytes, f64), IEEE 754. São pesquisados pelo mesmo
  mecanismo de varredura byte-a-byte, por **bytes exatos** (sem tolerância).
  O modo Auto inclui float e double juntamente com os tipos inteiros.
- O valor pesquisado/escrito pode ser inteiro (decimal, 0x, 0b, 0o) ou decimal com ponto
  ou vírgula (`1.5`, `1,5`, `1e3`). Um decimal ignora os tipos inteiros no modo Auto;
  `100.0` conta como inteiro.
- Limite de 16 milhões de candidatos por pesquisa.
- Apenas regiões de memória com permissão de leitura/escrita (sem devices nem guard pages).
- Máximo de 20 endereços mostrados na lista Found; valores iguais com larguras diferentes
  mostram-se apenas a mais estreita.
- Não há pesquisa de ponteiros nem de valor desconhecido (ver "Tarefas pendentes").

Memória e varredura
-------------------
- A varredura percorre todas as regiões RW do processo, com passo de 1 byte.
- Necessária para encontrar valores desalinhados (típico de jogos DOS em emuladores).
- Resultados podem ser truncados se ultrapassarem 16M de candidatos.
- Lock (cadeado) escreve o valor continuamente a cada 0,3 s (ou 1 s sem locks) para mantê-lo fixo.

Configuração
------------
Preferências guardadas em formato INI simples em
`~/.config/bsdconqueror/config.ini` (ou `$XDG_CONFIG_HOME/bsdconqueror/`).
É um ficheiro de texto simples. Para repor valores por omissão, apaga-o.

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
- O histórico guarda até 5 listas anteriores de candidatos (máx. 2M cada).
- `Undo` repõe a lista de candidatos da pesquisa anterior; os valores reais são lidos da memória.

Convenções de largura (scanner)
-------------------------------
- Código de largura: `1`, `2`, `4`, `8` = inteiros (tamanho em bytes); `5` = float32
  (4 bytes); `6` = double (8 bytes). O código NÃO é o tamanho em memória: usar
  `byte_len()` para isso.
- Os valores são do tipo `Num` (`Int(i64)`, `F32(f32)`, `F64(f64)`), definido em
  `src/core/scanner.rs`. `decode()` devolve `Num`; `pattern()` converte `Num` em bytes.
  `parse_num()` (em `src/ui/mod.rs`) converte o texto escrito pelo utilizador.
- Cada candidato é um `u64`: endereço nos 61 bits baixos e código de largura nos 3 bits
  altos (`TAG_SHIFT = 61`, `pack()`/`unpack()`). Não alargar o endereço nem a etiqueta
  sem rever estas duas funções.
- `ALL_WIDTHS` = `[1, 2, 4, 5, 6, 8]`, sem repetições.

Tarefas pendentes
-----------------

### Traduções em falta
- "Float 4 bytes", "Double 8 bytes" e "Auto (1, 2, 4, Float and Double)" só estão
  traduzidas em `po/pt.po`. Faltam as entradas em `po/es.po`, `po/de.po`, `po/fr.po`,
  `po/it.po` e `po/pl.po`. Sem elas o menu cai para o inglês nesses idiomas (não
  parte nada: `tr()` devolve a frase original).
- Cada `{}` da frase original tem de existir na tradução, pela mesma ordem; o teste
  `traducoes_tem_os_mesmos_marcadores` (`src/i18n.rs`) verifica-o.
- As mensagens "Type an integer value to search for." e "Enter a valid integer value to
  search for!" ainda dizem "integer", embora `parse_num()` aceite decimais. Alterar o
  texto implica atualizar os seis ficheiros `.po`.

### Pesquisa por valor desconhecido (por implementar)
Modo em que a primeira pesquisa não tem valor: guarda-se um instantâneo da memória e as
pesquisas seguintes filtram por "aumentou", "diminuiu", "mudou" ou "não mudou" (além de
"igual a"), repetindo até restarem poucos endereços.
- Sem valor inicial não há candidatos para filtrar. É preciso guardar o valor de cada
  endereço, não só o endereço como em `MemoryScanner::candidates` (`Vec<u64>`). Isto
  aumenta muito a memória usada.
- A pesquisa atual avança de 1 em 1 byte e limita-se a `MAX_CANDIDATES` (16 milhões).
  Neste modo isso não chega: alinhar ao tamanho do tipo e pesquisar um só tipo de cada vez.
- `Num` permite comparar inteiros e floats pelo mesmo caminho. `build_patterns()` e
  `perform_next_scan()` assumem comparação por bytes exatos; os modos novos comparam
  valores (`Num`), não bytes.
- O histórico de Undo (`remember()`, `HISTORY_MAX_CANDIDATES`) guarda só endereços e
  terá de ser adaptado para guardar os valores anteriores.
- Acrescentar o modo a `SCAN_NAMES`/`scan_widths()` (`src/ui/mod.rs`) e as frases novas
  aos seis `.po`.

### Testes
O scanner não tem testes. Cobrir `pattern()`, `decode()` e `pack()`/`unpack()`, que é
onde estavam os erros dos floats.

Licença
-------
Domínio público (Unlicense) para todo o código original.