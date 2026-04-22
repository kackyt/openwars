with open("engine/src/ai/production.rs", "r") as f:
    text = f.read()

import re

# Remove `println!("COMMANDS...`
text = re.sub(r'\s*println!\("COMMANDS: \?.*?;\n', '\n', text)

with open("engine/src/ai/production.rs", "w") as f:
    f.write(text)
