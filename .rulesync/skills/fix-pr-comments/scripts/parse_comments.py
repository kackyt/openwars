import subprocess
import json
import sys
import os

def run_cmd(cmd):
    result = subprocess.run(cmd, capture_output=True, text=True, encoding='utf-8')
    if result.returncode != 0:
        print(f"Command failed: {' '.join(cmd)}\n{result.stderr}")
        sys.exit(1)
    return result.stdout

def get_pr_info(pr_arg=None):
    if pr_arg:
        cmd = ['gh', 'pr', 'view', pr_arg, '--json', 'url,number,comments,reviews']
    else:
        cmd = ['gh', 'pr', 'view', '--json', 'url,number,comments,reviews']
        
    out = run_cmd(cmd)
    data = json.loads(out)
    
    url = data['url']
    number = data['number']
    general_comments = data.get('comments', [])
    reviews = data.get('reviews', [])
    for r in reviews:
        if r.get('body'):
            general_comments.append(r)
    
    parts = url.split('/')
    owner = parts[-4]
    repo = parts[-3]
    
    return owner, repo, number, general_comments

def fetch_threads(owner, repo, number):
    query = """
    query($name: String!, $owner: String!, $number: Int!, $cursor: String) {
      repository(owner: $owner, name: $name) {
        pullRequest(number: $number) {
          reviewThreads(first: 100, after: $cursor) {
            pageInfo {
              hasNextPage
              endCursor
            }
            nodes {
              isResolved
              isOutdated
              comments(first: 50) {
                nodes {
                  id
                  body
                  path
                  line
                  originalLine
                  author { login }
                  createdAt
                  diffHunk
                }
              }
            }
          }
        }
      }
    }
    """
    
    all_threads = []
    cursor = None
    
    while True:
        cmd = [
            'gh', 'api', 'graphql',
            '-F', f'owner={owner}',
            '-F', f'name={repo}',
            '-F', f'number={number}',
            '-f', f'query={query}'
        ]
        if cursor:
            cmd.extend(['-F', f'cursor={cursor}'])
            
        out = run_cmd(cmd)
        data = json.loads(out)
        
        pr_data = data.get('data', {}).get('repository', {}).get('pullRequest', {})
        threads_page = pr_data.get('reviewThreads', {})
        
        all_threads.extend(threads_page.get('nodes', []))
        
        page_info = threads_page.get('pageInfo', {})
        if page_info.get('hasNextPage'):
            cursor = page_info.get('endCursor')
        else:
            break
            
    result_data = {
        'data': {
            'repository': {
                'pullRequest': {
                    'reviewThreads': {
                        'nodes': all_threads
                    }
                }
            }
        }
    }
    return result_data

def generate_markdown(general_comments, threads_data):
    output_lines = ["# PR Review Comments Suggestions\n"]

    if general_comments:
        output_lines.append("## General PR Comments\n")
        for c in general_comments:
            author = c.get('author', {}).get('login') if c.get('author') else 'unknown'
            body = c.get('body', '').strip()
            if body:
                output_lines.append(f"### From @{author}\n{body}\n\n---\n")

    threads = threads_data.get('data', {}).get('repository', {}).get('pullRequest', {}).get('reviewThreads', {}).get('nodes', [])
    
    active_threads = []
    
    for thread in threads:
        if thread.get('isResolved'):
            continue  # Skip resolved comments
            
        comments = thread.get('comments', {}).get('nodes', [])
        if not comments:
            continue
            
        first_comment = comments[0]
        path = first_comment.get('path', 'Unknown file')
        line = first_comment.get('line') or first_comment.get('originalLine') or 'N/A'
        diff_hunk = first_comment.get('diffHunk', '')
        
        active_threads.append({
            'path': path,
            'line': line,
            'diff': diff_hunk,
            'comments': comments
        })

    if active_threads:
        output_lines.append("## Inline Suggestions by File\n")
        
        def sort_key(t):
            try:
                line_num = int(t['line']) if t['line'] != 'N/A' else 0
                return (t['path'], line_num)
            except ValueError:
                return (t['path'], 0)
                
        active_threads.sort(key=sort_key)
        
        for thread in active_threads:
            output_lines.append(f"### File: `{thread['path']}` (Line: {thread['line']})\n")
            if thread['diff']:
                output_lines.append("```diff\n" + thread['diff'] + "\n```\n")
            
            for tc in thread['comments']:
                author = tc.get('author', {}).get('login') if tc.get('author') else 'unknown'
                body = tc.get('body', '').strip()
                output_lines.append(f"**@{author}**: {body}\n\n")
            output_lines.append("---\n")

    output_path = "suggestions.md"
    with open(output_path, 'w', encoding='utf-8') as f:
        f.writelines(output_lines)
    
    print(f"Successfully generated {output_path}")

if __name__ == "__main__":
    pr_arg = sys.argv[1] if len(sys.argv) > 1 else None
    
    print("Fetching PR info...")
    owner, repo, number, general_comments = get_pr_info(pr_arg)
    
    print(f"Fetching review threads for {owner}/{repo}#{number}...")
    threads_data = fetch_threads(owner, repo, number)
    
    print("Generating suggestions.md...")
    generate_markdown(general_comments, threads_data)
