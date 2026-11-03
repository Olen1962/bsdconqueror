# BSDConqueror

Pesquisador e editor de memória de processos para **FreeBSD**, com interface gráfica
em Rust (egui/eframe). Funciona só com ferramentas **nativas do sistema**: `ps`,
`procstat` e `ptrace(2)`. Não precisa de `scanmem` nem de nenhum programa externo.

> Inspirado no GameConqueror/scanmem, mas escrito de raiz para o FreeBSD.
> Versões anteriores à 0.20 usavam o scanmem; a última delas está na etiqueta `scanmem-final`.

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
- Pesquisa de inteiros de **1, 2, 4 e 8 bytes** (por omissão "2 e 4 Bytes"; há também "Auto").
- Pesquisa em **todos os endereços**, byte a byte. Encontra valores desalinhados, típicos de
  jogos DOS dentro de emuladores (DOSBox e semelhantes).
- Lista **Found** com os primeiros 20 endereços distintos. Se o mesmo endereço aparece com
  várias larguras, mostra só a mais estreita.
- Valores **atualizados em tempo real** (de 1 em 1 s; de 0,3 em 0,3 s com locks ativos).
- **Lista de baixo** para os endereços de interesse:
  - editar o valor (duplo clique ou `F2`) e a descrição (duplo clique);
  - com várias linhas selecionadas, o valor e o lock aplicam-se a **todas**;
  - **Lock** (cadeado): escreve o valor continuamente para o manter fixo;
  - remover linhas (`Delete`), selecionar tudo (`Ctrl+A`), limpar a lista.
- Zoom da interface com **Ctrl + roda do rato** (também `Ctrl +`, `Ctrl -`, `Ctrl 0`).
- Preferências guardadas entre execuções: zoom, tamanho da janela, largura das colunas e
  dos painéis, tamanho de valor escolhido.

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

Usa sempre `--release`: a pesquisa percorre toda a memória byte a byte e em modo debug
fica muito mais lenta.

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
É um ficheiro de texto simples. Para repor os valores por omissão, apaga-o.

## Limitações conhecidas

- Só inteiros (sem `float`/`double` por enquanto).
- Valores de 1 byte entre 128 e 255 aparecem como negativos.
- Máximo de 16 milhões de endereços candidatos por pesquisa.
- Só percorre regiões de memória com leitura e escrita (sem dispositivos nem guard pages).
- Não há pesquisa de ponteiros nem de valores desconhecidos.

## Estrutura do projeto

```
src/
├── main.rs          ponto de entrada
├── core/
│   └── scanner.rs   ptrace, procstat, pesquisa e escrita de memória
├── ui/
│   └── mod.rs       interface (egui)
└── icons/           ícones PNG embebidos no executável
link_icons.sh        (opcional) copia ícones de um tema do sistema para src/icons
```

A interface não bloqueia: o scanner corre numa thread própria e comunica por canais.

## Próximos passos

- Desfazer (rollback) da última pesquisa.
- Menu (Ficheiro, Ver, Ajuda).
- Traduções com ficheiros `.po`.
- Pesquisa de `float` e `double`.

## Histórico

| Versão | Descrição |
|---|---|
| 0.20.0 | Scanner nativo (`ptrace`/`procstat`), sem scanmem |
| 0.12.x | Versão com scanmem (etiqueta `scanmem-final`) |

## Licença

**Uso 100% livre.** Este projeto é de **domínio público** (licença [Unlicense](LICENSE)):

- qualquer pessoa pode copiar, editar, alterar, compilar, publicar, vender ou distribuir o código,
  para qualquer fim, comercial ou não;
- **não é preciso referir créditos**, nem o autor, nem este projeto;
- **sem garantias** de qualquer tipo: o programa é fornecido "tal como está" e os autores não
  respondem por quaisquer danos, incluindo perda de dados ou de saves.

Exceções, que mantêm as suas próprias licenças:

- as bibliotecas usadas (egui/eframe e outras), que se obtêm com o `cargo`;
- os ícones em `src/icons`, que vêm de temas de ícones do sistema e do GameConqueror. Para o
  projeto ficar totalmente livre, podem ser substituídos por ícones próprios.
