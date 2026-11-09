import json
import sys

request = json.load(sys.stdin)
query = request.get("query", "").strip()
config = request.get("config", {})
greeting = config.get("greeting", "你好")
if config.get("uppercase", False):
    greeting = greeting.upper()
punctuation = config.get("punctuation", "无")
suffix = "" if punctuation == "无" else punctuation
repeat = config.get("repeat", 1)
message_text = " ".join([f"{greeting}，{query or '世界'}{suffix}"] * repeat)
if request["action"] == "search":
    reply = {"results": [{"id": "greet", "title": message_text, "detail": "按 Enter 显示问候"}]}
elif request["action"] == "activate":
    message = "这是 Take a Panel 插件示例。" if request.get("id") == "about" else message_text
    reply = {"message": message, "keep_open": True}
else:
    raise ValueError("unknown action")
json.dump(reply, sys.stdout, ensure_ascii=False)
