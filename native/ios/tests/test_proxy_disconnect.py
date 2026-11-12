"""以真實 socket 驗證媒體代理在用戶端斷線後不會被 SIGPIPE 終止。"""
from pathlib import Path
import os
import subprocess
import tempfile
import platform

root = Path(__file__).resolve().parents[1]
proxy = (root / "App/Proxy.swift").read_text()
api = (root / "App/Api.swift").read_text()
allow = api[api.index("func trustedOrigin("):api.index("private func scriptSrc(")]
harness = r'''
extension MediaProxy {
    func testSend(_ fd: Int32, _ payload: Data) { sendAll(fd, payload) }
}
// 還原系統預設動作，避免測試執行器忽略 SIGPIPE 而掩蓋問題。
signal(SIGPIPE, SIG_DFL)
let proxy = MediaProxy()
let payload = Data("media fragment".utf8)
var fds: [Int32] = [-1, -1]
precondition(socketpair(AF_UNIX, SOCK_STREAM, 0, &fds) == 0)
proxy.testSend(fds[0], payload)
var buffer = [UInt8](repeating: 0, count: payload.count)
let received = buffer.withUnsafeMutableBytes { recv(fds[1], $0.baseAddress, $0.count, 0) }
precondition(received == payload.count && Data(buffer) == payload)
close(fds[1])
// 模擬切換分頁後播放器關閉連線，代理仍在回傳已下載的分片。
for _ in 0..<100 { proxy.testSend(fds[0], payload) }
close(fds[0])
print("PASS: 正常傳輸與用戶端斷線後持續送出皆不會終止程式")
'''
with tempfile.TemporaryDirectory() as tmp:
    source = Path(tmp) / "main.swift"
    binary = Path(tmp) / "proxy-disconnect"
    error = api[api.index("enum ApiError:"):api.index("final class HgApi")]
    source.write_text("import Combine\n" + proxy + "\n" + error + allow + harness)
    env = dict(os.environ)
    subprocess.run(["xcrun", "swiftc", "-swift-version", "5", "-target", f"{platform.machine()}-apple-macosx13.0",
                    str(source), "-o", str(binary)], env=env, check=True)
    result = subprocess.run([str(binary)], capture_output=True, text=True)
    print(result.stdout, end="")
    assert result.returncode == 0, f"代理測試失敗，結束碼 {result.returncode}（-13 表示 SIGPIPE）\n{result.stderr}"
