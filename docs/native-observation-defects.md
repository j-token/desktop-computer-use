# 네이티브 관측 경로 결함 분석

이 문서는 비공개 원시 측정에서 확인한 접근성 관측 경로 결함 세 가지를 기록한 과거 설계 자료다. 원시 화면과 프로토콜 기록은 개인정보 보호를 위해 공개 저장소에 포함하지 않는다. 각 결함은 증상, 원인, 근거, 고칠 곳 순서로 정리했고, 인용한 코드는 문서를 작성할 때 직접 확인한 소스에서 옮겼다.

측정 자체는 사전 빌드된 `.cache/package-bin/win32-x64/desktop-computer-use-native.exe`로 했고, 아래 결함 1·2·3은 `build-baseline/Release/desktop-computer-use-native.exe`로 다시 빌드해 같은 소스에서 재현했다. 이 문서를 쓰는 동안 `native/`는 다른 에이전트가 동시에 수정하고 있었다. 각 결함 항목 끝에 "현재 상태" 줄을 두어, 그 사실을 감추지 않고 확인된 대로 적었다.

## 결함 1 — 64 KiB를 넘는 응답이 전달되지 않는다

**증상**: 접근성 요소가 많은 창(Chrome YouTube 페이지, Orca, System32 폴더, 파일 40/80/120/160개 폴더)에서 `get-app-state --include-text`를 호출하면 클라이언트가 바이트를 0개 받은 채 15초 뒤 EPIPE로 실패한다. 반면 49,223바이트 이하 응답은 전부 성공했다.

**원인**: `native/common/server.cpp:192`가 파이프를 만들 때 `PIPE_NOWAIT`과 65,536바이트짜리 출력 버퍼를 함께 쓴다.

```cpp
HANDLE pipe = CreateNamedPipeW(name.c_str(), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS, 32, 65536, 65536, 0, &security.attributes);
```

`write_response`(118번째 줄)는 남은 응답 전체 길이를 한 번의 `WriteFile`에 그대로 넘긴다.

```cpp
void write_response(Connection connection, const std::string& response) {
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(15);
    std::size_t bytesSent = 0;
    while (bytesSent < response.size() && std::chrono::steady_clock::now() < deadline) {
        const auto bytesWritten = write_bytes(connection, response.data() + bytesSent,
                                              static_cast<int>(response.size() - bytesSent));
        if (bytesWritten == 0) break;
        if (bytesWritten < 0) {
            std::this_thread::sleep_for(streamRetryInterval);
            continue;
        }
        bytesSent += bytesWritten;
    }
```

`write_bytes`(40번째 줄)는 논블로킹 `WriteFile`이 0바이트를 쓰면 그 결과를 `-1`로 매핑한다.

```cpp
int write_bytes(Connection connection, const char* buffer, int size) {
    DWORD written = 0;
    if (!WriteFile(connection, buffer, size, &written, nullptr)) return 0;
    return written ? static_cast<int>(written) : -1;
}
```

남은 응답 길이가 파이프의 빈 공간보다 크면 `WriteFile`은 0바이트를 쓰고, `write_bytes`는 이를 `0`이 아니라 `-1`로 돌려주므로 `write_response`의 루프는 `bytesWritten < 0` 분기를 타 2ms 자고 같은(줄지 않은) 길이로 재시도한다. 응답 크기가 애초에 빈 버퍼보다 크면 이 재시도는 15초 마감까지 반복되다가, 결국 `if (bytesWritten == 0) break;`(원래 코드에서는 이 분기가 조용히 실행돼) 아무것도 보내지 못한 채 연결을 닫는다.

**근거**: `daemon.log`는 실패가 반복되는 동안에도 비어 있었다. 원시 프로토콜 트레이스에서 성공 사례는 125ms에 49,223바이트짜리 청크 하나로 끝났고, 실패 사례는 15,141ms에 `total=0 chunks=0`으로 끝났다 — 잘린 프레임이 아니라 바이트가 아예 0개였다. 실패 동안 데몬 CPU는 초당 약 0.08초만 증가해(유휴 틱 수준) 무거운 작업이 아니라 재시도 루프에서 잠들어 있었음을 보여준다. `skills/desktop-computer-use/references/protocol.md`가 문서화한 16 MiB 프레임 상한(`maximumFrameBytes`, 24번째 줄)은 실제로는 도달한 적이 없다. 실제 천장은 파이프 버퍼 크기인 약 64 KiB이고, 이번 측정에서 나온 요소당 약 365바이트(탐색기 49,223바이트/135요소)를 대입하면 대략 175개 요소가 한계다. 이 결함이 지금까지 드러나지 않은 이유는, 스크린샷 응답은 픽셀 대신 파일 경로(`screenshot.path`)만 실어 보내 1KB 안팎이었고, 접근성 경로는 `tests/integration/benchmark-observation.mjs`가 `includeText: false`를 고정해(37번째 줄) 어떤 기록된 검증 실행에서도 실행된 적이 없었기 때문이다.

**고칠 곳**: 매 `WriteFile` 호출 길이를 파이프 버퍼보다 충분히 작게 제한해야 한다.

**현재 상태**: 이 문서를 쓰는 동안 다시 열어 보니 `native/common/server.cpp`에 `writeChunkBytes = 16 * 1024` 상수가 추가되어 각 `WriteFile` 호출이 16KB로 제한되어 있었고, `bytesWritten == 0`일 때 `"transport: response truncated at ... bytes"`를 `std::cerr`로 남기고 종료하도록 바뀌어 있었다. 청크 크기를 줄이면 한 번의 빈 버퍼보다 큰 쓰기를 시도할 가능성은 줄어들지만, 여전히 첫 0바이트 쓰기에서 재시도 없이 `break`하므로 파이프가 계속 막혀 있는 상황까지 완전히 없앤다고 확인하지는 못했다. 이 수정이 이번 측정에서 재현한 실패를 실제로 없애는지는 별도로 다시 측정해야 한다.

## 결함 2 — 관측이 창을 활성화하지 않는다

**증상**: 백그라운드 상태에서 `get-app-state`를 호출하면 요소 수가 한 자릿수로 잡힌다(설정 1개, 탐색기 8개, Radmin VPN 1개). 같은 창을 활성화한 뒤 다시 호출하면 요소 수가 크게 늘어난다(설정 121개, 탐색기 135개). Radmin VPN만은 활성화해도 여전히 1개였다.

**원인**: `native/windows/windows_backend.cpp`의 `observe()`(747번째 줄에서 시작)는 `activate()`를 호출하지 않는다.

```cpp
Json WindowsBackend::observe(const Json& params, Context& context) {
    purge_observations();
    const auto target = require_target_window(params);
    const bool observeText = json_bool(params, "includeText", false) ||
                             json_string(params, "observe") == "text" ||
                             json_string(params, "observe") == "both";
```

반면 입력 행동들은 `activate(target.hwnd, context)`를 명시적으로 호출한다. 같은 파일에서 `activate(`가 실제로 호출되는 지점은 다음과 같다: 850번째 줄(`click_at` 내부), 883번째 줄(`drag_at` 내부), 1027번째 줄(`click`의 elementIndex 경로), 1069번째 줄(`scroll`), 1090번째 줄(`type-text`), 1095번째 줄(`press-key`/`hotkey`), 1104번째 줄(`set-value`), 1111번째 줄(`paste-text`). `observe()`만 이 목록에서 빠져 있다. 백그라운드로 밀린 UWP/WinUI 창은 UIA 트리를 접는다.

**근거**: 위 포그라운드 대 배경 표. 설정은 1개에서 121개로, 탐색기는 8개에서 135개로 늘었다.

**관측 후 바로 행동하는 호출자에 대한 결과**: 판단은 접힌 트리를 기준으로 이뤄지고, 창은 행동을 보낼 때가 돼서야 앞으로 나온다. 관측 시점과 행동 시점의 트리가 서로 다른 상태를 반영한다는 뜻이다.

**고칠 곳**: 관측 결과에 포그라운드 상태를 함께 보고하고, 옵트인 활성화를 제공하되 기본 동작은 그대로 두는 방향이 맞다.

**현재 상태**: 이 문서를 쓰는 동안 다시 열어 보니 `observe()`에 다음 분기가 추가되어 있었다.

```cpp
if (json_bool(params, "activate", false)) {
    // Backgrounded UWP/WinUI windows collapse their UIA tree, so an opt-in
    // activation exists.  It restores the window and can move it, so the
    // rectangle every coordinate below is built from must be re-read.
    activate(target.hwnd, context);
    if (const auto activated = inspect_window(target.hwnd)) target = *activated;
}
```

`params`에 `activate: true`를 넘길 때만 활성화하고 기본값은 `false`로 남아 있어, 위 "고칠 곳"에서 적은 방향과 정확히 같은 모양으로 구현되어 있었다. 다만 이 옵트인 파라미터가 CLI나 스킬 문서에도 노출되어 있는지는 이번 세션에서 확인하지 않았다.

## 결함 3 — 넓이가 0인 요소가 창 모서리를 누른다

**증상**: 좌표 폴백으로 요소를 클릭할 때, UIA가 바운딩 사각형을 못 주는 요소는 창의 좌상단 모서리(또는 화면 (0,0))를 클릭한다.

**원인**: `native/windows/uia.cpp`의 `get_bounds`(114번째 줄)는 바운딩 사각형 프로퍼티를 못 가져오면 0으로 채운 `RECT`를 그대로 돌려준다.

```cpp
RECT get_bounds(IUIAutomationElement* element, const RECT& windowRect) {
    RECT result{};
    VARIANT value;
    VariantInit(&value);
    if (SUCCEEDED(element->GetCachedPropertyValue(UIA_BoundingRectanglePropertyId, &value)) &&
        value.vt == (VT_ARRAY | VT_R8) && value.parray) {
        ...
    }
    VariantClear(&value);
    return result;
}
```

프로퍼티 조회가 실패하면 `if` 블록에 들어가지 않으므로 `result`는 기본 초기화된 `RECT{}`, 즉 전부 0인 사각형이다. `native/windows/windows_backend.cpp`의 요소 클릭 폴백은 이 사각형을 그대로 평균 낸다.

```cpp
if (!uia_.invoke(record, context)) {
    point.x = target.rect.left + (record.bounds.left + record.bounds.right) / 2;
    point.y = target.rect.top + (record.bounds.top + record.bounds.bottom) / 2;
    click_at(target.hwnd, point, mouse_button(json_string(params, "button")), params, context);
}
```

`bounds`가 전부 0이면 이 평균도 0이 되어, 창 기준 좌표 (0,0) — 즉 창의 좌상단 모서리 — 를 클릭한다. `uia_.invoke()`를 먼저 시도해 패턴을 가진 요소는 이 경로를 타지 않으므로, 영향을 받는 것은 좌표 폴백뿐이다.

**근거**: 이번 측정에서 조작 가능 요소로 분류되지 않은 항목들(설정 55개, 탐색기 2개, 카카오톡 5개가 이름 없음 또는 패턴 없음/오프스크린으로 제외됨) 가운데 바운딩 사각형이 0인 요소가 섞여 있는지는 이번 세션에서 요소 단위로 하나씩 대조하지 않았다. 코드 경로상 결함이 존재함은 위 인용으로 확인했지만, 실제 클릭 오발생 사례를 재현해 캡처하지는 못했다.

**고칠 곳**: 이상 사각형에서 좌표를 만들어내지 말고, 타입이 있는 오류를 돌려주는 방향이 맞다.

**현재 상태**: 이 문서를 쓰는 동안 다시 열어 보니 `windows_backend.cpp`에 `require_clickable_bounds`가 추가되어 있었다.

```cpp
// A degenerate bounding rectangle would centre on the window corner, so every
// coordinate derived from an element has to refuse it instead of clicking there.
void require_clickable_bounds(const ElementRecord& record) {
    if (record.bounds.right <= record.bounds.left || record.bounds.bottom <= record.bounds.top) {
        throw Error("element_not_actionable",
                    "Element has no clickable area; observe again or choose another element");
    }
}
```

이 함수가 `click`의 elementIndex 폴백과 `drag`의 요소 좌표 계산 앞에 삽입되어, 이상 사각형에서 좌표를 만들기 전에 `element_not_actionable` 오류를 던지도록 되어 있었다. 위 "고칠 곳"과 같은 방향이다. 다만 `uia.cpp`의 `get_bounds` 자체는 여전히 0으로 채운 `RECT`를 조용히 돌려준다 — 근본 원인은 남아 있고, 호출자 쪽에서만 그 결과를 걸러내는 형태다.

## 결함 4 — 지원하지 않는 패턴까지 전부 보고한다

### 증상

`get-app-state`가 돌려주는 모든 요소의 `patterns`가 예외 없이 `["invoke","value","select","toggle"]` 네 가지를 전부 담고 있었다. 창도, 이름 없는 장식용 이미지도, 스크롤바도 마찬가지였다.

### 근거

측정 당시 기록해 둔 네 개 앱을 세어 보니 한 건의 예외도 없었다.

| 고정 자료 | 요소 수 | 네 패턴을 모두 가진 요소 |
|---|---|---|
| 계산기 | 57 | 57 |
| Chrome | 275 | 275 |
| 메모장 | 74 | 74 |
| 탐색기 | 136 | 136 |

수정 뒤 같은 계산기 창을 다시 관측하니 `["invoke"]` 35개, `[]` 19개, `["select"]` 2개, `["value"]` 1개로 나뉘었다. 패턴이 없는 요소는 창, 컨테이너, 메뉴 항목이었고 `invoke`를 가진 것은 버튼이었다.

### 원인

`native/windows/uia.cpp`의 `cached_pattern()`이 `GetCachedPattern`의 반환 코드만 확인했다.

```cpp
bool cached_pattern(IUIAutomationElement* element, PATTERNID pattern) {
    ComPtr<IUnknown> unknown;
    return SUCCEEDED(element->GetCachedPattern(pattern, &unknown));
}
```

이 API는 요소가 해당 패턴을 지원하지 않아도 성공을 돌려주고 인터페이스 포인터만 null로 둔다. 따라서 반환 코드는 "캐시에 물어볼 수 있었다"는 뜻일 뿐이고, 지원 여부는 포인터가 결정한다. 조건에 `&& unknown`을 더해 고쳤다.

같은 파일의 `invoke()`는 `GetCurrentPattern` 뒤에 `unknown.As(...)`로 실제 인터페이스를 얻어 쓰기 때문에 이 결함의 영향을 받지 않았다. 잘못된 값이 나간 곳은 관측 응답의 `patterns` 배열뿐이다.

### 이 필드에 기대는 쪽에 미치는 영향

`patterns`는 "이 요소를 조작할 수 있는가"와 "어떤 조작을 제안할 것인가"를 정하는 유일한 근거다. 모든 요소가 모든 패턴을 가진 것으로 보이면 그 구분이 통째로 사라진다.

실제로 확인된 사례가 하나 있다. 탐색기 검색창은 `Edit`(index 104)이고 그 안에 같은 이름 " 다운로드 검색"을 가진 안내 문구 `Text`(index 105)가 겹쳐 있다. 수정 전 기록에서는 둘 다 네 패턴을 다 가진 것으로 보여 구분할 수 없었고, 겹친 요소를 정리하는 단계가 안쪽의 안내 문구를 남기고 진짜 검색창을 버렸다. 수정 뒤에는 `Edit`가 `["value"]`, `Text`가 `[]`로 갈려 검색창이 남았고 역할도 글자를 넣을 수 있는 칸으로 바로잡혔다.

### 이 결함이 드러나지 않은 이유

결함 1과 같다. 접근성 트리를 포함한 응답 경로가 이 저장소의 어떤 기존 검증에서도 실행된 적이 없었다. `patterns`는 값이 나간 적이 없으니 틀렸는지도 확인된 적이 없었다.

## 부수 관찰

**drag의 활성화 여부**: 원래 자료에는 "drag가 activate()를 호출하지 않는 유일한 동작"이라는 설명이 있었으나, 소스를 직접 확인한 결과와 다르다. `handle_action`의 `method == "drag"` 분기 자체는 `activate(`를 직접 호출하지 않지만, 그 분기가 호출하는 `drag_at()` 함수의 첫 줄이 `activate(hwnd, context);`다.

```cpp
void WindowsBackend::drag_at(HWND hwnd, POINT from, POINT to, MouseButton button,
                             int duration, int steps, int holdBefore, int holdAfter,
                             Context& context) {
    activate(hwnd, context);
```

즉 drag도 다른 입력 행동과 마찬가지로 창을 활성화한다. 활성화를 거치지 않는 행동은 이번 조사에서 찾지 못했다. 이 점은 원 설명을 그대로 옮기지 않고 정정해서 적는다.

**actionTransform 미구현**: `actionTransform`은 `skills/desktop-computer-use/references/protocol.md`(24, 25, 30번째 줄)와 `skills/desktop-computer-use/SKILL.md`(26번째 줄)에 명시되어 있다.

```text
protocol.md:24  `actionX = (pixelX - offsetX) / scaleX` and `actionY = (pixelY - offsetY) / scaleY`. If the
protocol.md:25  observation has no `actionTransform`, use `scaleX`/`scaleY` when present and otherwise `scale`; an
protocol.md:30  `actionTransform`, as well as an accessibility element list, timings, and overlay regions. MCP reads
SKILL.md:26     if `screenshot.actionTransform` is present, use `actionX = (pixelX - offsetX) / scaleX` and
```

`src/`와 `native/` 전체를 `actionTransform` 문자열로 검색했지만 일치하는 코드가 없었다. 문서가 약속하는 필드를 실제로 채워 보내는 코드는 없다는 뜻이다. 이 결과는 이 문서 작성 시점의 `src/`, `native/` 상태를 대상으로 한 것이며, 위 결함 1·2·3과 달리 이번 세션 안에서 변경을 다시 확인하지는 않았다.

> **해결됨:** 2단계 스크린샷 변경으로 Windows와 Linux 관찰 결과의 `screenshot`(축소본)과 `get-full-screenshot` 결과(원본)가 모두 `actionTransform`을 채운다. 좌표 입력은 daemon이 이 변환으로 창 좌표를 계산하므로, 위에 인용한 수동 계산 지침은 SKILL.md와 protocol.md에서 삭제했다.
