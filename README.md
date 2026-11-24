# Cinema Online

Самостоятельный подключаемый модуль (плагин) онлайн-просмотра для **Cinema**.
Не зависит от ядра приложения, компилируется под Windows, Linux, macOS и Android TV, и может размещаться в отдельном репозитории на GitHub.

## Состав модуля

- **Нативный Rust-плагин (`cinema-online` crate)** — встроенные провайдеры:
  - `collaps` — фильмы и сериалы в HLS (до 1080p) с множеством аудиодорожек и субтитров.
  - `rezka` — фильмы и сериалы с поддержкой озвучек, сезонов, личных зеркал и `Cookie`.
  - `kodik` — фильмы, сериалы и аниме.
  - `remote` — подключение собственных HTTP-серверов по протоколу Cinema Online API.
  - `manifest` — динамическое подключение и обновление источников через `manifest.json` (например, с GitHub).

## Подключение к Cinema

1. В `Cargo.toml` проекта Cinema подключите crate локально или напрямую с GitHub:
   ```toml
   cinema-online = { git = "https://github.com/killnaive/cinema-online", optional = true }
   ```
   *(либо локально: `path = "../../plugins/cinema-online"`)*
2. Включите модуль в интерфейсе Cinema: **Настройки → Онлайн-модуль → Модуль онлайн-просмотра**.
3. Чтобы обновить зеркала или подключить набор источников без пересборки приложения на любом устройстве (ПК, сервер, Android TV), вставьте URL манифеста в разделе подключения плагина:
   ```
   https://raw.githubusercontent.com/killnaive/cinema-online/main/manifest.json
   ```

## Протокол Cinema Online Manifest (`manifest.json`)

```json
{
  "id": "my-online-pack",
  "name": "My Sources",
  "version": "0.1.0",
  "description": "Описание набора источников",
  "sources": [
    {
      "id": "collaps",
      "name": "Collaps",
      "kind": "collaps",
      "url": "https://api.nextembed.ws",
      "mirror": "https://api.kinogram.best",
      "token": "",
      "enabled": true
    }
  ]
}
```

## Протокол удалённого сервера Cinema (`kind = "remote"`)

Удалённый сервер реализует 3 JSON-эндпоинта:

- `POST {url}/providers` — принимает `TitleQuery`, возвращает `{ "providers": [{ "name", "balancer", "url" }] }`.
- `POST {url}/browse` — принимает `{ "url" }`, возвращает `{ "voices": [...], "items": [...], "message"?: "..." }`.
- `POST {url}/resolve` — принимает `{ "url" }`, возвращает `{ "url", "hls", "headers": {}, "qualities": [...], "subtitles": [...] }`.
