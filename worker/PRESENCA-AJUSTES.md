# Resposta do autor — ajustes da presença

Tarefa 10, branch `gpt/worker-presenca`. Relatório recebido:
`claude/revisao-worker-presenca:docs/revisoes/worker-presenca.md`, revisão de `48c0f8a`.
Este documento registra a resposta do GPT; o veredito final continua sendo do Claude.

## 1. Transporte do lugar na sessão

`seated_since_ms` é obrigatório, `null` ou inteiro seguro não negativo. Foi incluído no parser, na migração 0003,
no upsert, no GET e nos retratos devolvidos pelo POST. A 0003 ainda não foi aplicada remotamente, conforme a evidência recebida.
Os testes reais de SQLite verificam valores nulos, zero, limite seguro, alteração do lugar e preservação em anúncios recusados.

## 2. Relógio global e reinício do app

SPEC e README agora exigem AppClock em milissegundos para `online_since_ms` e `seated_since_ms`.
O horário de recebimento do Worker continua determinando a validade, separado da ordenação anunciada pelos clientes.
O upsert compara `(online_since_ms, seq)` atomicamente: execução nova com horário maior pode reiniciar a sequência imediatamente;
um pacote atrasado da execução anterior não substitui a execução atual, mesmo com sequência maior.
Depois de expirar, qualquer par válido é aceito. O teste de reinício verifica também a preservação de lugar e validade nos pacotes recusados.

## 3. Orçamento do D1

Adotei as três medidas sugeridas na revisão:

- heartbeat de 30 segundos e TTL de 90 segundos;
- POST devolve os retratos completos dos grupos atuais do chamador, inclusive grupos vazios;
- migração 0003 não cria o índice de expiração de presença.

GET continua disponível para consultas explícitas. O cliente não precisa de GET periódico em paralelo com POST.
Cada grupo aplica os mesmos filtros de associação, frescor e disponibilidade; um grupo não recebe identidades de quem está ocupado em outro.
Um único SELECT obtém todos os retratos. Um teste com 60 grupos verifica que o número de consultas não cresce por grupo;
outro simula 8 dispositivos com dois intervalos e verifica 16 registros de anti-replay para 16 POSTs, sem GET adicional.

O SPEC registra uma estimativa conservadora de até 8 linhas por heartbeat, incluindo exclusão futura do anti-replay
e manutenção da chave primária da presença. São 7680 linhas/hora com oito dispositivos e 92160 em 12 horas.
Isso reserva apenas parte do limite diário: chamadas de clipes, cadastros, mudanças imediatas, expiração final de presença
e mais dispositivos também contam. Não é uma garantia de cobrança nem suporte a 24 horas contínuas no plano grátis.
Os números precisam ser comparados às métricas reais no D1.
Referência verificada: [preços e índices do D1](https://developers.cloudflare.com/d1/platform/pricing/).

O cliente nativo deverá configurar `presence_ttl_ms = 90000` e enviar anúncios imediatamente após mudanças de sessão,
como exige o contrato da sessão. O Worker também informa `heartbeat_interval_ms` na resposta.

## 4. Retratos completos no adaptador

SPEC e README registram que o adaptador Worker → SessionManager deve remover quem sumiu do retrato,
sem renovar a validade quando o par de execução/sequência se repete. Isso vale para POST e GET.
O transporte/adaptador nativo permanece pendente na Fase C; não alterei o crate pertencente ao Claude.

## Verificação

- Typecheck: passou.
- Worker: 363 testes passaram, incluindo 51 de presença; 8 novos casos nesta rodada.
- Rust: fmt, Clippy com `-D warnings`, testes do workspace e check de todos os targets Windows GNU passaram.
- Os testes usam dados sintéticos e SQLite local. Nenhuma captura, migração D1 remota ou publicação foi executada.
