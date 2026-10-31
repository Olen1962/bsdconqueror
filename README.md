# BSDConqueror o 'cheat game' para BSD

Aplicação gráfica em Rust com **egui** para pesquisa e manipulação de memória em processos no FreeBSD, utilizando o `scanmem` como motor de backend.

## Pré-requisitos essenciais (FreeBSD)

Para que o `scanmem` consiga ler e escrever na memória dos processos, o sistema de ficheiros **linprocfs** tem de estar montado na camada de compatibilidade Linux. 

Certifique-se de que tem a seguinte entrada configurada no seu `/etc/fstab`:

```text
linprocfs         /compat/linux/proc  linprocfs  rw        0       0
