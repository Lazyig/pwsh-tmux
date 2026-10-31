# 터미널 UI/UX 비교 및 수정 — 2026-10-31

tmux 3.6a와 프로젝트를 격리된 서버·실제 PTY/ConPTY에서 실행하여 52개 상태를
비교했다. 설정 파일은 비우고 터미널 크기·종류와 상태줄 시각을 같게 했다.
실제 키 입력, 마우스, 터미널 셀, 보이는 커서, 입력 모드, 복사 결과와 CLI
완료 시점을 검사했다. 이미지와 HTML은 셀 데이터의 재구성이며 OS 스크린샷은 아니다.

| 최종 조건 | 화면 일치 | 동적 pane ID만 다른 화면 | 기타 화면 차이 | 명령 오류 |
|---|---:|---:|---:|---:|
| Linux, 한글·CJK·결합 문자 포함, 52개 | 51 | 1 | 0 | 0 |
| Windows, ASCII 대조군, 52개 | 51 | 1 | 0 | 0 |
| Windows, 한글·CJK·결합 문자 포함, 52개 | 32 | 별도 분리 안 함 | 20 | 0 |

모든 조건에서 내부 상태, 복사 결과, application-cursor·bracketed-paste 등
입력 모드, 모달 CLI 대기 여부는 일치했다. 기본 메뉴의 `%0`과 `%3`은 각 서버가
발급한 실제 pane ID이므로 값을 바꾸지 않았다. 비교기는 그 한 메뉴의 ID 토큰만
허용하며 원시 화면 차이는 보고서에 남긴다. NFC로 같은 문자열, 숨긴 커서의 좌표와
눈에 보이지 않는 빈칸 전경색은 가시적 차이에서 제외한다.

## 수정한 차이

- 명령 프롬프트의 불필요한 공백, 입력 시작점과 긴 입력의 스크롤.
- 연결된 클라이언트에 CLI 메시지 표시, 메시지·번호 오버레이의 커서 숨김.
- 복사 모드의 행·열 좌표, 점진 검색 취소 위치, 검색 결과 수, 선택 색과 검색 강조 해제.
- 활성 테두리의 색 분할, 복사 모드·동기 입력 시 테두리 색, 숫자 테두리,
  위·아래 pane 제목줄의 공간·접합부·제목 위치.
- 메뉴의 중앙 위치, 제목 정렬, 구분선 접합부, 선택·비활성 표시와 기본 Swap Marked 항목.
- 팝업의 중앙 위치와 제목 여백. 메뉴·팝업·번호 명령의 종료까지 CLI 대기,
  교체·종료 시 큐 재개 및 after 훅 실행. 대기 큐는 64개로 제한하고 재개 시 권한을 재검사한다.
- 트리·버퍼 선택기의 기본 목록 구조, 선택 강조, 미리보기 상자·내용·pane 번호.
- 좁은 상태줄의 오른쪽 끝 보존과 현재 윈도우 중심 생략, 큰 시계의 세로 위치.
- 옵션 형식에서 `off`를 거짓인 `0`으로 평가. 테두리의 기본 조건식도 원본과 같게 했다.
- 리사이즈 시 정상 화면의 줄 재배치와 화면 지우기 시 이력 보존. `scroll-on-clear`
  옵션을 추가했다. 로컬 vt100 변경 내역은 `vendor/vt100/PATCHES.md`에 기록했다.

복사 화면 비교 전에는 양쪽에 `clear-history`를 실행한다. 리사이즈에 반응하는
프로그램의 비동기 다시 그리기 횟수에 따라 과거 화면 수가 달라지는 영향을 분리하기
위함이다. 이력 보존, 줄 재배치, 스타일·넓은 문자 보존, 이력 상한과 대체 화면은
별도의 회귀 테스트로 검증한다.

## Windows Unicode 차이의 범위

기존 프로젝트의 결합 문자 출력은 다음 줄의 첫 글자를 밀어냈다. Windows에서
조합 가능한 셀을 NFC로 출력하고 논리적 다음 열을 복구하여 수정했다. 수정 후
기본 화면은 원본과 NFC 기준으로 일치한다.

Windows의 52개 Unicode 비교에는 WSL 원본 → ConPTY와 Windows 프로그램 → ConPTY라는
서로 다른 경로가 포함된다. 원본 쪽에서 분할·테두리 다시 그리기 후 결합 문자가 있는
행의 선이나 글자가 사라지는 현상이 기록되었으며, ASCII 대조군에는 없다.
프로젝트 출력에서 이 현상을 재현하도록 만들지는 않았다. 20열 화면의 마지막 열에도
Windows pane의 넓은 문자 줄바꿈 처리와 Linux pane 사이에 한 셀 차이가 남는다.
이 조건의 20개 차이를 모두 해결했다고 주장하지 않는다. 별도 Unicode 보고서에
원시 셀과 화면을 보존했다. 한글 IME·폰트·DPI·모든 Windows 터미널 조합의 동일성도
이 검사로 보장하지 않는다.

읽기 전용 권한 제한, 자원 상한, Windows 기본 셸과 IPC 방식은 의도적인 차이로
유지한다. UI 수정이 해당 보안 경계를 완화하지 않도록 별도 UID 검사와 큐 권한
회귀 테스트를 실행했다.

## 증거와 재현

- `target/ui-final/report.html`: Linux·Windows ASCII 화면과 차이 셀.
- `target/ui-final/summary.json`: 52개씩의 집계.
- `target/ui-unicode-final/report.html`: Windows Unicode의 별도 비교.
- 각 조건의 `reference.json`, `candidate.json`, `differences.json`, 주요 화면 PNG.

```powershell
$env:PT_UI_OUT = 'target/ui-final/windows-ascii'
$env:PT_UI_FIXTURE_SUFFIX = '--ascii'
cargo test --locked --offline --test ui_compat -- --ignored --nocapture
python tools/ui_report.py target/ui-final --check
```

`PT_UI_REFERENCE`로 원본 실행 파일, `PT_UI_CANDIDATE`로 검사할 실행 파일을 지정할 수
있다. Linux에서는 네이티브 tmux 3.6a 경로를 지정한다. 기록기 종료 코드 0은 기록 완료만
의미한다. `ui_report.py --check`는 조건별 52개 기록의 완성도와 화면·상태·모드·복사·명령
차이를 검사하여 예상하지 않은 차이가 있으면 실패한다. 보고서 PNG 생성에는 Pillow와
Windows Consolas/Malgun 폰트를 사용한다.
