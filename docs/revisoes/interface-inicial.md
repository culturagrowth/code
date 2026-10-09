# Revisão — tarefa nº 24: interface inicial do app

- Branch: `gpt/interface-inicial` · commit revisado: `2b7eeb90e7abd7d75e06c14728fba3ca7e1cf324` (implementação `4858586`, base `ee48246`)
- Implementado por: GPT · Revisado por: Claude · Data: 2026-10-09 · critério leve (decisão 19)
- Checagens refeitas pelo Claude num worktree próprio:
  - `npm test` (apps/desktop): **4 passaram**.
  - `npm run test:browser` (Edge headless, só a página de teste, nenhuma captura da área de trabalho): **passou** com os 9 checks
    (rotas, reprodução de MP4 real, busca, nome como texto, remover sem apagar o arquivo, exportação, recarga das preferências,
    layout mobile, **nenhuma requisição externa**). No ambiente do GPT este teste não rodava; aqui rodou.
  - O TOML exportado pelo teste foi aceito pelo gravador real: `duoclip-recorder --check-config` → `(OK)` (só lê, não grava).
  - Rust não foi alterado (só `apps/desktop/` e uma decisão na memória); não repeti as checagens Rust já registradas pelo GPT.

## Conferido
- Servidor de prévia só em `127.0.0.1`, lista fixa de arquivos, CSP que bloqueia `connect-src`, sem acesso a segredos do repositório.
- Nenhum estado fictício: "Gravador não conectado", grupos sem conexão inventada, biblioteca só com arquivos escolhidos pelo usuário.
- Visual (capturas da página do teste): painel escuro coerente, textos em português, layout limpo; boa base para o shell Tauri.

## Achados (menores, para depois — não pedem nova rodada)
- **UI-1:** o atalho padrão da interface é `Alt+F10` (mesmo padrão do gravador), que colide com a NVIDIA (HK-1). O usuário usa `F10`.
  Quando a ligação nativa existir, a tela deve ler o `config.toml` real em vez de mostrar os padrões.
- **UI-2:** a duração exibida no início ("40 s") é antes + depois; com extensões o clipe fica maior. Rótulo talvez "Clipe padrão".

## Veredito
**Aprovado.**
