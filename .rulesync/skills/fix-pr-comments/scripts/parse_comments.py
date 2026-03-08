import json
import sys
import os

def parse_pr_comments(json_path):
    if not os.path.exists(json_path):
        print(f"Error: {json_path} not found.")
        return

    with open(json_path, 'r', encoding='utf-8') as f:
        try:
            data = json.load(f)
        except json.JSONDecodeError as e:
            print(f"Error decoding JSON: {e}")
            return

    output_lines = ["# PR Review Comments Suggestions\n"]

    # --- General Comments ---
    comments = data.get('comments', [])
    if comments:
        output_lines.append("## General PR Comments\n")
        for c in comments:
            author = c.get('author', {}).get('login', 'unknown')
            body = c.get('body', '').strip()
            if body:
                output_lines.append(f"### From @{author}\n{body}\n\n---\n")

    # --- Inline Review Comments ---
    # We group by path and line to show conversations clearly
    reviews = data.get('reviews', [])
    threads = {}

    for review in reviews:
        review_comments = review.get('comments', [])
        for rc in review_comments:
            # gh CLI structure for review comments
            path = rc.get('path', 'Unknown file')
            line = rc.get('line') or rc.get('originalLine', 'N/A')
            diff_hunk = rc.get('diffHunk', '')
            
            # Create a unique key for the thread (naive approach: path + line)
            key = f"{path}:{line}"
            if key not in threads:
                threads[key] = {
                    'path': path,
                    'line': line,
                    'diff': diff_hunk,
                    'comments': []
                }
            
            threads[key]['comments'].append({
                'author': rc.get('author', {}).get('login', 'unknown'),
                'body': rc.get('body', '').strip(),
                'createdAt': rc.get('createdAt', ''),
                'id': rc.get('id', '')
            })

    if threads:
        output_lines.append("## Inline Suggestions by File\n")
        # Sort by path then line
        sorted_keys = sorted(threads.keys())
        for key in sorted_keys:
            thread = threads[key]
            output_lines.append(f"### File: `{thread['path']}` (Line: {thread['line']})\n")
            if thread['diff']:
                output_lines.append("```diff\n" + thread['diff'] + "\n```\n")
            
            # Sort comments by date
            thread_comments = sorted(thread['comments'], key=lambda x: x['createdAt'])
            for tc in thread_comments:
                output_lines.append(f"**@{tc['author']}**: {tc['body']}\n\n")
            output_lines.append("---\n")

    output_path = "suggestions.md"
    with open(output_path, 'w', encoding='utf-8') as f:
        f.writelines(output_lines)
    
    print(f"Successfully generated {output_path}")

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python parse_comments.py <pr_comments.json>")
    else:
        parse_pr_comments(sys.argv[1])
