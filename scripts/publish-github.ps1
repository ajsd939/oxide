# Creates or updates the GitHub repo for this project WITHOUT git.exe -
# everything goes through `gh api` against the Git Data API
# (blobs -> tree -> commit -> ref). Safe to re-run: it appends a new commit
# on top of the existing branch.
#
# Usage: powershell -ExecutionPolicy Bypass -File scripts/publish-github.ps1 [-Message "..."]

param(
    [string]$Repo = "ajsd939/rustide",
    [string]$Branch = "main",
    [string]$Message = "Rust IDE 0.3.0 - Tauri + Monaco IDE (LSP intellisense, terminal, git, builds)"
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Split-Path -Parent $PSScriptRoot)).Path.TrimEnd('\', '/')

$excludeDirs = @("target", "monaco", "xterm", ".git", "node_modules")
$files = Get-ChildItem -Path $root -Recurse -File | Where-Object {
    $rel = $_.FullName.Substring($root.Length).TrimStart('\', '/')
    $parts = $rel -split '[\\/]'
    -not ($parts | Where-Object { $excludeDirs -contains $_ })
}

Write-Host "Publishing $($files.Count) files to $Repo ($Branch)"

# 1. Create blobs for each file.
$entries = @()
$i = 0
foreach ($f in $files) {
    $i++
    $rel = $f.FullName.Substring($root.Length).TrimStart('\', '/') -replace '\\', '/'
    $b64 = [Convert]::ToBase64String([IO.File]::ReadAllBytes($f.FullName))
    $body = @{ content = $b64; encoding = "base64" } | ConvertTo-Json -Compress
    $sha = $body | gh api "repos/$Repo/git/blobs" --method POST --input - --jq ".sha"
    if (-not $sha) { throw "blob failed for $rel" }
    $entries += @{ path = $rel; mode = "100644"; type = "blob"; sha = $sha }
    Write-Host ("  [{0}/{1}] {2}" -f $i, $files.Count, $rel)
}

# 2. Resolve the parent commit (if the branch already exists).
$parentSha = $null
$baseTree = $null
$prevEap = $ErrorActionPreference
$ErrorActionPreference = "Continue"   # native stderr (409 on empty repo) must not throw
$existingRaw = gh api "repos/$Repo/git/ref/heads/$Branch" 2>$null
$ErrorActionPreference = $prevEap
if ($existingRaw) {
    $existing = $existingRaw | ConvertFrom-Json
    if ($existing -and $existing.object.sha) {
        $parentSha = $existing.object.sha
        $parentCommit = gh api "repos/$Repo/git/commits/$parentSha" | ConvertFrom-Json
        $baseTree = $parentCommit.tree.sha
    }
}

# 3. Create the tree.
$treeBody = @{ tree = $entries }
if ($baseTree) { $treeBody.base_tree = $baseTree }
$treeSha = ($treeBody | ConvertTo-Json -Depth 6) | gh api "repos/$Repo/git/trees" --method POST --input - --jq ".sha"
if (-not $treeSha) { throw "tree creation failed" }

# 4. Create the commit.
$commitBody = @{ message = $Message; tree = $treeSha; parents = @() }
if ($parentSha) { $commitBody.parents = @($parentSha) }
$commitSha = ($commitBody | ConvertTo-Json -Depth 6) | gh api "repos/$Repo/git/commits" --method POST --input - --jq ".sha"
if (-not $commitSha) { throw "commit creation failed" }

# 5. Point the branch at it (create or fast-forward).
if ($parentSha) {
    $refBody = @{ sha = $commitSha; force = $false } | ConvertTo-Json
    $refBody | gh api "repos/$Repo/git/refs/heads/$Branch" --method PATCH --input - --jq ".object.sha" | Out-Null
} else {
    $refBody = @{ ref = "refs/heads/$Branch"; sha = $commitSha } | ConvertTo-Json
    $refBody | gh api "repos/$Repo/git/refs" --method POST --input - --jq ".object.sha" | Out-Null
}

Write-Host ""
Write-Host "Published $commitSha -> https://github.com/$Repo/tree/$Branch"
