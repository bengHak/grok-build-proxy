# grok-build-proxy

Grok Build의 네이티브 **OpenAI Responses API** 요청을 ChatGPT Codex 백엔드로
전달하는 로컬 프록시입니다. Claude/Anthropic 프로토콜 변환 없이 Grok Build의
에이전트, 도구 호출, 세션 UI를 Codex 모델과 함께 사용할 수 있게 합니다.

> 비공식 커뮤니티 프로젝트입니다. OpenAI, ChatGPT, Codex, xAI 또는 Grok과
> 제휴하거나 이들 회사가 보증하는 프로젝트가 아닙니다. ChatGPT 계정과
> 워크스페이스에 허용된 모델만 사용할 수 있으며 내부 엔드포인트 변경으로
> 호환성이 깨질 수 있습니다.

## 목차

- [동작 방식](#동작-방식)
- [빠른 시작](#빠른-시작)
- [Grok Build 설정](#grok-build-설정)
- [지원 모델과 Fast 별칭](#지원-모델과-fast-별칭)
- [Responses Lite 변환](#responses-lite-변환)
- [설정](#설정)
- [보안](#보안)
- [개발](#개발)
- [제약 사항](#제약-사항)

## 동작 방식

```text
Grok Build
  POST /v1/responses (standard Responses API)
          │
          ▼
grok-build-proxy
  - Codex CLI auth.json 로드 및 OAuth 갱신
  - ChatGPT-Account-ID 등 Codex 헤더 주입
  - GPT-5.6 계열을 Responses Lite 형식으로 변환
  - SSE 응답을 바이트 단위로 스트리밍
          │
          ▼
ChatGPT Codex Responses backend
```

프록시는 다음 엔드포인트만 제공합니다.

| Endpoint | 설명 |
|---|---|
| `POST /v1/responses` | Codex 요청 프록시 |
| `GET /v1/models` | Grok Build용 모델 목록 |
| `GET /healthz` | 프로세스 상태 |
| `GET /readyz` | Codex 인증 파일을 포함한 준비 상태 |

`/responses`와 `/models`도 호환 별칭으로 동작합니다.

## 빠른 시작

### 1. 빌드

Go 1.23 이상이 필요합니다.

```bash
git clone https://github.com/bengHak/grok-build-proxy.git
cd grok-build-proxy
make build
```

생성된 실행 파일은 `bin/grok-build-proxy`입니다.

### 2. 전용 Codex 로그인 디렉터리 준비

동일한 refresh token을 여러 프로세스가 동시에 갱신하는 상황을 피하기 위해
전용 `CODEX_HOME` 사용을 권장합니다.

```bash
export CODEX_HOME="$HOME/.codex-grok-build-proxy"
mkdir -p "$CODEX_HOME"
cat > "$CODEX_HOME/config.toml" <<'EOF'
cli_auth_credentials_store = "file"
EOF
codex login
```

브라우저를 사용할 수 없는 환경에서는 공식 Codex CLI의 장치 코드 로그인을
사용할 수 있습니다.

```bash
CODEX_HOME="$HOME/.codex-grok-build-proxy" codex login --device-auth
```

로그인이 끝나면 `$CODEX_HOME/auth.json`이 있어야 합니다. 이 파일은 액세스 및
리프레시 토큰을 포함하므로 비밀번호처럼 취급해야 합니다.

### 3. 프록시 시작

```bash
CODEX_HOME="$HOME/.codex-grok-build-proxy" \
  ./bin/grok-build-proxy
```

기본 주소는 `http://127.0.0.1:18765`입니다.

```bash
curl --fail http://127.0.0.1:18765/readyz
```

## Grok Build 설정

[`examples/grok-config.toml`](examples/grok-config.toml)의 원하는 블록을
`~/.grok/config.toml`에 병합합니다. 최소 예시는 다음과 같습니다.

```toml
[model.codex-terra]
model = "gpt-5.6-terra"
name = "Codex GPT-5.6 Terra"
base_url = "http://127.0.0.1:18765/v1"
api_backend = "responses"
api_key = "unused"
context_window = 372000
```

`api_key = "unused"`는 Grok Build가 xAI 세션 토큰을 이 로컬 엔드포인트에
사용하지 않도록 하기 위한 값입니다. 프록시는 루프백 바인딩일 때 들어오는
Authorization 값을 사용하지 않으며, 실제 Codex 인증은 Codex CLI의
`auth.json`에서 읽습니다.

실행:

```bash
grok -m codex-terra
```

설정 초안을 자동으로 출력할 수도 있습니다.

```bash
./bin/grok-build-proxy --print-grok-config
```

## 지원 모델과 Fast 별칭

기본 모델 카탈로그는 다음 모델을 노출합니다.

| 모델 | 컨텍스트 창 | 전송 형식 |
|---|---:|---|
| `gpt-5.6-sol` | 372,000 | Responses Lite |
| `gpt-5.6-terra` | 372,000 | Responses Lite |
| `gpt-5.6-luna` | 372,000 | Responses Lite |
| `gpt-5.5` | 272,000 | Responses |
| `gpt-5.2` | 272,000 | Responses |

실제 모델 접근 가능 여부는 ChatGPT 플랜, 워크스페이스 정책, 지역 및 서버 측
롤아웃에 따라 달라집니다. 프록시 목록에 존재한다고 계정 접근 권한이 생기는
것은 아닙니다.

모델 ID 뒤에 `-fast`를 붙이면 프록시가 접미사를 제거하고
`service_tier = "priority"`를 추가합니다.

```toml
[model.codex-sol-fast]
model = "gpt-5.6-sol-fast"
name = "Codex GPT-5.6 Sol (Fast)"
base_url = "http://127.0.0.1:18765/v1"
api_backend = "responses"
api_key = "unused"
context_window = 372000
```

Fast 티어 지원 여부와 사용량 영향은 계정 및 모델에 따라 달라질 수 있습니다.

내장 목록을 바꾸려면 쉼표로 구분해 전달합니다. 목록에 없는 모델 ID도 요청
자체는 차단하지 않으므로 새 모델을 먼저 시험할 수 있습니다.

```bash
GROK_BUILD_PROXY_MODELS="gpt-5.6-sol,gpt-5.6-terra" \
  ./bin/grok-build-proxy
```

## Responses Lite 변환

현재 Codex 카탈로그의 GPT-5.6 Sol/Terra/Luna는 Responses Lite 전송 형식을
사용합니다. Grok Build는 표준 Responses API를 생성하므로 프록시가 다음을
자동 변환합니다.

- 최상위 `tools`를 `additional_tools` 개발자 입력 항목으로 이동
- `instructions`를 개발자 메시지로 이동
- `reasoning.context = "all_turns"` 설정
- `parallel_tool_calls = false` 설정
- Responses Lite 헤더와 client metadata 추가
- 반환되는 Responses SSE 이벤트는 변환 없이 Grok Build로 스트리밍

GPT-5.5와 GPT-5.2 같은 일반 Responses 모델은 요청 구조를 유지한 채 인증
헤더만 추가합니다.

## 설정

| Flag | Environment | Default |
|---|---|---|
| `--listen` | `GROK_BUILD_PROXY_LISTEN` | `127.0.0.1:18765` |
| `--auth-file` | `GROK_BUILD_PROXY_AUTH_FILE` | `$CODEX_HOME/auth.json` 또는 `~/.codex/auth.json` |
| `--upstream` | `GROK_BUILD_PROXY_UPSTREAM` | ChatGPT Codex Responses endpoint |
| `--refresh-url` | `GROK_BUILD_PROXY_REFRESH_URL` | OpenAI OAuth token endpoint |
| `--models` | `GROK_BUILD_PROXY_MODELS` | 내장 모델 카탈로그 |
| `--client-token` | `GROK_BUILD_PROXY_TOKEN` | 없음 |
| `--log-format` | `GROK_BUILD_PROXY_LOG_FORMAT` | `text` |

### 비루프백 바인딩

`0.0.0.0`이나 LAN 주소에 바인딩하려면 반드시 들어오는 요청용 bearer token을
설정해야 합니다.

```bash
export GROK_BUILD_PROXY_TOKEN="replace-with-a-long-random-value"
./bin/grok-build-proxy --listen 0.0.0.0:18765
```

이 경우 Grok Build 설정의 `api_key`를 같은 값으로 지정합니다.

## Docker

```bash
docker build -t grok-build-proxy .
docker run --rm \
  --user "$(id -u):$(id -g)" \
  -p 127.0.0.1:18765:18765 \
  -v "$HOME/.codex-grok-build-proxy:/home/proxy/.codex:rw" \
  grok-build-proxy --listen 0.0.0.0:18765 \
    --auth-file /home/proxy/.codex/auth.json \
    --client-token local-container-token
```

컨테이너 내부에서는 비루프백 주소로 수신하므로 `--client-token`이 필요합니다.
Grok Build 설정에도 같은 토큰을 `api_key`로 넣습니다.

## 보안

- 기본값처럼 루프백 주소에서만 실행하는 것을 권장합니다.
- `auth.json`을 Git, Docker 이미지, 로그, 이슈 또는 채팅에 올리지 마세요.
- 프록시는 요청/응답 본문과 Authorization 헤더를 로그로 남기지 않습니다.
- 공용 인터넷에 직접 노출하지 마세요.
- 자동화 환경에서는 OpenAI가 제공하는 공식 API 키 또는 허가된 Codex access
  token 방식이 더 적합할 수 있습니다.
- 자세한 내용은 [`SECURITY.md`](SECURITY.md)를 참고하세요.

## 개발

외부 Go 모듈 없이 표준 라이브러리만 사용합니다.

```bash
make check
```

개별 명령:

```bash
go test -race ./...
go vet ./...
go build ./cmd/grok-build-proxy
```

## 제약 사항

- Codex CLI가 자격 증명을 OS keyring에만 저장한 경우 읽을 수 없습니다.
  `cli_auth_credentials_store = "file"`로 전용 `CODEX_HOME`을 준비하세요.
- ChatGPT Codex 백엔드는 공개 OpenAI Platform API와 별개의 제품 경로이며,
  서버 측 변경에 따라 프록시 업데이트가 필요할 수 있습니다.
- 현재 구현은 HTTP Responses/SSE 경로를 사용하며 Codex의 WebSocket 전송은
  구현하지 않습니다.
- xAI의 서버 측 검색 도구가 아니라 Grok Build가 로컬에서 실행하는 함수
  도구를 대상으로 합니다. Codex의 hosted search 도구 호환성은 보장하지
  않습니다.

## 참고 자료

- [OpenAI Codex authentication documentation](https://learn.chatgpt.com/docs/auth)
- [OpenAI Codex open-source repository](https://github.com/openai/codex)
- [xAI Grok Build repository](https://github.com/xai-org/grok-build)
- [raine/claude-code-proxy](https://github.com/raine/claude-code-proxy)
