# Interface do DuoClip

Primeira interface: início, biblioteca, amigos/grupos e configurações. Funciona sem instalar pacotes.

Na pasta deste arquivo:

```powershell
npm.cmd run dev
```

Abra **http://127.0.0.1:1420** no navegador. Para encerrar, Ctrl+C no terminal. Não é necessário `npm install`.

- Adicione MP4 pelo botão ou arrastando para a biblioteca. Os arquivos ficam no computador; a lista dura até fechar/recarregar a página.
- Clique em um clipe para reproduzir. Retirar da lista mantém o arquivo original.
- Salve suas preferências no navegador e use **Baixar config.toml** para exportar a configuração completa.
- Gravação e amigos ainda não estão conectados à interface. Não existe gravação automática ao abrir a página.

Para aplicar a configuração exportada ao gravador, confira primeiro os valores. Você pode testar e usar o arquivo escolhido sem substituir a configuração anterior:

```powershell
# No repositório, substitua o caminho pelo arquivo que acabou de baixar:
cargo run -p duoclip-recorder -- --config 'C:\caminho\config.toml' --check-config
# Para iniciar o gravador quando quiser gravar jogo/áudio:
cargo run -p duoclip-recorder -- --config 'C:\caminho\config.toml'
```

Os sons personalizados precisam existir no PC. A exportação não altera o gravador já aberto. Presets e padrão Alt+F10 seguem o gravador atual; se outro app usar o atalho, escolha outro.

Validação:

```powershell
npm.cmd test
npm.cmd run test:browser
```

O teste de navegador usa Edge já instalado e FFmpeg para um MP4 sintético. Não instala nada nem grava a tela. Capturas apenas da página vão para `test-output/interface-inicial/` do worktree. Consulte [SPEC.md](SPEC.md) para os limites e a integração futura com Tauri 2.
