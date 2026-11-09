import json
import sys

request = json.load(sys.stdin)
query = request.get("query", "").strip()
config = request.get("config", {})
greeting = config.get("greeting", "你好")
if request["action"] == "search":
    reply = {"results": [{"id": "greet", "title": f"{greeting}，{query or '世界'}", "detail": "按 Enter 显示问候"}]}
elif request["action"] == "activate":
    message = "这是 Take a Panel 插件示例。" if request.get("id") == "about" else f"{greeting}，{query or '世界'}"
    reply = {"message": message, "keep_open": True}
else:
    raise ValueError("unknown action")
json.dump(reply, sys.stdout, ensure_ascii=False)
