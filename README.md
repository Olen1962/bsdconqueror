# BSDConqueror

Pesquisador e editor de memória de processos para **FreeBSD**, com interface gráfica
em Rust (egui/eframe). Funciona só com ferramentas **nativas do sistema**: `ps`,
`procstat` e `ptrace(2)`. Não precisa de `scanmem` nem de nenhum programa externo.

> Inspirado no GameConqueror/scanmem, mas escrito de raiz para o FreeBSD.

## Para que serve

Encontrar onde um programa guarda um valor (vidas, moedas, pontos...) e alterá-lo
ou fixá-lo ("congelá-lo"). O fluxo é o habitual:

1. Escolher o processo.
2. Pesquisar um valor.
3. Mudar o valor no programa e usar **Next Search** para ir estreitando os resultados.
4. Passar os endereços finais para a lista de baixo e editar ou fixar o valor.

Usa-o em programas **teus** (jogos de um jogador, por exemplo). Não o uses em jogos
online nem em programas de outras pessoas.

## Funcionalidades

- Lista de processos com filtro e ordenação por coluna (PID, utilizador, nome).
- Pesquisa de inteiros de **1, 2, 4 e 8 bytes**, **float** (4 bytes) e **double** (8 bytes)
  (por omissão "2 e 4 Bytes"; há também "Auto").
- Pesquisa por **valor desconhecido**: o primeiro passo guarda um instantâneo da memória
  e os seguintes filtram por aumentou, diminuiu, mudou, não mudou ou igual a.
- Pesquisa em **todos os endereços**, byte a byte, no modo exato. Encontra valores
  desalinhados, típicos de jogos DOS dentro de emuladores (DOSBox e semelhantes). No modo
  de valor desconhecido a pesquisa é alinhada ao tamanho do tipo escolhido.
- Lista **Found** com os primeiros 20 endereços distintos. Se o mesmo endereço aparece com
  várias larguras, mostra só a mais estreita.
- Valores **atualizados em tempo real** (de 1 em 1 s; de 0,3 em 0,3 s com locks ativos).
- **Desfazer** a última pesquisa (até 5 listas anteriores).
- **Lista de baixo** para os endereços de interesse:
  - editar o valor (duplo clique ou `F2`) e a descrição (duplo clique);
  - com várias linhas selecionadas, o valor e o lock aplicam-se a **todas**;
  - **Lock** (cadeado): escreve o valor continuamente para o manter fixo;
  - remover linhas (`Delete`), selecionar tudo (`Ctrl+A`), limpar a lista.
- Zoom da interface com **Ctrl + roda do rato** (também `Ctrl +`, `Ctrl -`, `Ctrl 0`).
- Preferências guardadas entre execuções: zoom, tamanho da janela, largura das colunas e
  dos painéis, tipo de pesquisa, idioma e tema.

## Requisitos

- FreeBSD (testado em 14.x) com `ps` e `procstat` (fazem parte do sistema base).
- Ambiente gráfico (X11 ou Wayland).
- Rust recente (1.85 ou superior recomendado) e `cargo`.

## Compilar e executar

```sh
git clone git@github.com:Olen1962/bsdconqueror.git
cd bsdconqueror
cargo build --release
./target/release/bsdconqueror
```

Ou, usando o `Makefile` (compatível com `bmake`/FreeBSD e GNU make):

```sh
make
./target/release/bsdconqueror
```

Usa sempre `--release` (ou `make`): a pesquisa percorre toda a memória byte a byte e em
modo debug fica muito mais lenta.

## Instalar (FreeBSD)

Para instalar o binário e a integração de desktop (ícone e ficheiro `.desktop`):

```sh
make
make install
```

Por omissão instala em `/usr/local`. Podes ajustar o prefixo e usar `DESTDIR` para staging:

```sh
make PREFIX=/usr install
make DESTDIR=/tmp/pkg PREFIX=/usr/local install
```

Para desinstalar:

```sh
make uninstall
```

## Permissões

- Para processos **do mesmo utilizador** não é preciso root, desde que
  `security.bsd.unprivileged_proc_debug=1` (é o valor por omissão no FreeBSD).
- Para processos de **outros utilizadores** (incluindo `root`), ou se esse sysctl estiver a 0,
  executa o programa como root.

Se aparecer `PT_ATTACH falhou ... Operation not permitted`, falta uma destas permissões.

## Como funciona e o que isso implica

O programa anexa-se ao processo com `ptrace` (`PT_ATTACH`), lê e escreve a memória com
`PT_IO` e desanexa-se logo a seguir.

- **O processo fica parado** enquanto está anexado. A primeira pesquisa pode demorar vários
  segundos e o programa alvo fica congelado nesse tempo.
- Com locks ativos há uma paragem muito curta a cada 0,3 s.
- Alterar memória de um programa pode estragá-lo. Faz cópias de segurança dos teus saves.
- Escrever 4 bytes num valor que só ocupa 2 bytes também altera os 2 bytes seguintes, que
  podem pertencer a outra variável. Em caso de dúvida, usa **2 bytes**.

## Atalhos

| Ação | Atalho |
|---|---|
| Pesquisar | `Enter` na caixa Value |
| Editar valor da linha | duplo clique em Value, ou `F2` |
| Editar descrição | duplo clique em Description |
| Selecionar tudo (lista de baixo) | `Ctrl+A` |
| Remover linhas selecionadas | `Delete` |
| Desselecionar | `Esc` |
| Zoom | `Ctrl` + roda do rato |

## Preferências

Ficam em `~/.config/bsdconqueror/config.ini` (ou em `$XDG_CONFIG_HOME/bsdconqueror/`).
É um ficheiro de texto simples. Para repor os valores por omissão, elimina-o.

## Limitações conhecidas

- Valores de 1 byte entre 128 e 255 aparecem como negativos.
- Máximo de 16 milhões de endereços candidatos por pesquisa exata. O modo de valor
  desconhecido tem um limite próprio (8 milhões) e guarda também o valor de cada endereço,
  pelo que usa bastante mais memória.
- Só percorre regiões de memória com leitura e escrita (sem dispositivos nem guard pages).
- **Sem pesquisa de ponteiros (pointer scan):** O programa não suporta resolução
  de cadeias de ponteiros multinível. Para o público-alvo principal no FreeBSD —
  emuladores como o **DOSBox**, ScummVM e jogos retro —, esta funcionalidade não é
  necessária, pois estes ambientes operam em blocos de memória contíguos e estáticos
  (sem ASLR nem alocações dinâmicas complexas típicas de motores 3D modernos).
  A varredura byte-a-byte desalinhada e o modo de valor desconhecido cobrem
  praticamente todas as necessidades nestes cenários.

## Estrutura do projeto

```
src/
├── main.rs          ponto de entrada
├── core/
│   └── scanner.rs   ptrace, procstat, pesquisa e escrita de memória
├── ui/
│   └── mod.rs       interface (egui)
└── icons/           ícones PNG embebidos no executável (e svg/ com os originais)
tools/
└── make_icons.py    gera os ícones (PNG e SVG); precisa de Python 3 e Pillow
```

A interface não bloqueia: o scanner corre numa thread própria e comunica por canais.

## Próximos passos

- Suporte a inteiros sem sinal (unsigned, exibição de `0..255` em 1 byte).
- Testes de integração com um processo alvo real (o `ptrace` não corre em testes unitários).

## Histórico

| Versão | Descrição |
|---|---|
| 0.30.0 | Float/double, desfazer, traduções e pesquisa por valor desconhecido |
| 0.20.0 | Scanner nativo (`ptrace`/`procstat`), sem scanmem |

## Licença

**Uso 100% livre.** O trabalho original deste projeto (código, ícones e documentação) é de
**domínio público** (licença [Unlicense](LICENSE)):

- qualquer pessoa pode copiar, editar, alterar, compilar, publicar, vender ou distribuir,
  para qualquer fim, comercial ou não;
- **não é preciso referir créditos**;
- **sem garantias** de qualquer tipo: o programa é fornecido "tal como está" e os autores não
  respondem por quaisquer danos, incluindo perda de dados ou de saves.

Tudo o que seja de terceiros mantém os direitos e as licenças de origem, sejam adquiridos,
herdados ou de qualquer outra natureza. É o caso das bibliotecas obtidas com o `cargo`
(egui/eframe e outras), que não fazem parte deste trabalho e têm as suas próprias licenças.
