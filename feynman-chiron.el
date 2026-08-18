;;; feynman-chiron.el --- Feynman Technique learning with AI -*- lexical-binding: t; -*-

;; Copyright (C) 2025

;; Author: Valentyn
;; Version: 2.1.1
;; Package-Requires: ((emacs "27.1") (transient "0.3.0"))
;; Keywords: learning, education, ai
;; URL: https://github.com/kamysh/feynman-chiron

;;; Commentary:

;; Feynman Chiron implements the Feynman Technique for active learning.
;;
;; You explain concepts in your own words, Chiron identifies gaps,
;; asks probing questions, and helps you refine until you truly understand.
;;
;; This file is the Emacs frontend only.  It drives a separate `chiron-rs`
;; binary (see chiron-rs/ in this repo, or a release download) as a
;; subprocess for retrieval, LLM calls, and PostgreSQL-backed mastery
;; tracking.  See README.md for installation and configuration.
;;
;; Usage:
;;   M-x feynman-chiron-start   (enables `feynman-chiron-mode' here)
;;
;; Write your explanation freely under an org heading — the heading is
;; the concept, its subtree is your explanation, and nothing is ever
;; locked read-only: revise it in place like any other org text.
;;   - C-c C-c to submit the current subtree
;;   - Chiron's response is inserted as a `Chiron: ' turn, followed by a
;;     `You: ' prompt for your reply
;;   - Each submit sends the WHOLE transcript so far (never stripped or
;;     truncated) — write your reply after `You: ' and press C-c C-c
;;

;;; Code:

(require 'auth-source)
(require 'json)
(require 'lisp-mnt)
(require 'transient)
(require 'org)
(require 'org-id)
(require 'org-element)

(defconst feynman-chiron--package-dir
  (file-name-directory
   (or load-file-name buffer-file-name (locate-library "feynman-chiron")))
  "Directory this package was loaded from.
Captured once at load time: `load-file-name' is only bound while a
file is actively being loaded, so it cannot be read reliably from
inside a function body called later (e.g. from `M-x').")

;;; Customization

(defgroup feynman-chiron nil
  "Feynman Technique learning with AI."
  :group 'applications
  :prefix "feynman-chiron-")


(defcustom feynman-chiron-default-provider 'anthropic
  "Default API provider: openai or anthropic.
`openai' speaks the OpenAI chat-completions wire format, so it also
covers any OpenAI-compatible endpoint (DeepSeek, Together, OpenRouter,
Groq, Mistral, Ollama, ...) when paired with `feynman-chiron-endpoint-url'.
Can be overridden per-buffer with feynman-chiron-provider."
  :type '(choice (const :tag "OpenAI-compatible (OpenAI, DeepSeek, Together, OpenRouter, Groq, ...)" openai)
                 (const :tag "Anthropic" anthropic))
  :group 'feynman-chiron)

(defvar-local feynman-chiron-provider nil
  "API provider for this buffer: openai or anthropic.
If nil, uses feynman-chiron-default-provider.
Set via file-local variables:
  # Local Variables:
  # feynman-chiron-provider: openai
  # End:")
(put 'feynman-chiron-provider 'safe-local-variable
     (lambda (v) (memq v '(nil openai anthropic))))

(defcustom feynman-chiron-openai-model "gpt-4"
  "Default OpenAI model."
  :type 'string
  :group 'feynman-chiron)

(defcustom feynman-chiron-anthropic-model "claude-sonnet-4-6"
  "Default Anthropic model."
  :type 'string
  :group 'feynman-chiron)

(defvar-local feynman-chiron-model nil
  "Model to use for this buffer.
If nil, uses provider default.
Set via file-local variables:
  # Local Variables:
  # feynman-chiron-model: \"gpt-4-turbo\"
  # End:")
(put 'feynman-chiron-model 'safe-local-variable #'stringp)

;;; API keys

(defcustom feynman-chiron-openai-key nil
  "OpenAI API key: a string, or a zero-arg function returning one.
A function is called fresh on every use, so it can wrap
`password-store-get', a shell-out to a credential manager, etc.
If nil, looked up lazily from `auth-source' (host \"api.openai.com\")."
  :type '(choice (const :tag "Look up via auth-source" nil)
                 (string :tag "API Key")
                 (function :tag "Function returning the key"))
  :group 'feynman-chiron)

(defcustom feynman-chiron-anthropic-key nil
  "Anthropic API key: a string, or a zero-arg function returning one.
A function is called fresh on every use, so it can wrap
`password-store-get', a shell-out to a credential manager, etc.
If nil, looked up lazily from `auth-source' (host \"api.anthropic.com\")."
  :type '(choice (const :tag "Look up via auth-source" nil)
                 (string :tag "API Key")
                 (function :tag "Function returning the key"))
  :group 'feynman-chiron)

(defun feynman-chiron--auth-source-key (host)
  "Look up a secret for HOST via `auth-source'."
  (let ((secret (plist-get (car (auth-source-search :host host :max 1)) :secret)))
    (if (functionp secret) (funcall secret) secret)))

(defun feynman-chiron--resolve-key (key host)
  "Resolve KEY to a string: call it if a function, else use as-is.
Falls back to an `auth-source' lookup on HOST when KEY is nil."
  (cond ((functionp key) (funcall key))
        (key key)
        (t (feynman-chiron--auth-source-key host))))

(defun feynman-chiron--openai-key ()
  "Return the configured OpenAI API key, falling back to `auth-source'."
  (feynman-chiron--resolve-key feynman-chiron-openai-key "api.openai.com"))

(defun feynman-chiron--anthropic-key ()
  "Return the configured Anthropic API key, falling back to `auth-source'."
  (feynman-chiron--resolve-key feynman-chiron-anthropic-key "api.anthropic.com"))

;;; Internal variables

(defvar-local feynman-chiron-backend-process nil
  "Chiron agent backend process for this buffer.")

;;; API Communication

(defun feynman-chiron--get-provider ()
  "Get the API provider for current buffer."
  (or feynman-chiron-provider
      feynman-chiron-default-provider))

(defun feynman-chiron--model ()
  "Get model name for the current provider."
  (let ((provider (feynman-chiron--get-provider)))
    (or feynman-chiron-model
        (if (eq provider 'openai)
            feynman-chiron-openai-model
          feynman-chiron-anthropic-model))))

;;; Rust Backend Integration

(defcustom feynman-chiron-backend-program nil
  "Path to the chiron-rs binary.
If nil, looks for \\='chiron-rs\\=' on PATH and in the package directory."
  :type '(choice (const :tag "Auto-detect" nil)
                 (file :tag "Path to binary"))
  :group 'feynman-chiron)

(defcustom feynman-chiron-endpoint-url nil
  "Base URL override for the configured LLM provider.
For provider \\='openai\\=' (OpenAI-compatible wire format), overrides the
default https://api.openai.com — use for Groq, Mistral, Ollama, or any
other OpenAI-compatible endpoint. Examples:
  Groq:   https://api.groq.com/openai
  Ollama: http://localhost:11434/v1
For provider \\='anthropic\\=', overrides the default
https://api.anthropic.com — use for a local Anthropic-API-compatible
proxy such as Meridian, e.g. http://localhost:3456.
If nil, each provider uses its own real API endpoint."
  :type '(choice (const :tag "Default" nil)
                 (string :tag "Endpoint URL"))
  :group 'feynman-chiron)

(defvar-local feynman-chiron-database-url nil
  "Base PostgreSQL database URL.
Set via direnv (.envrc), .dir-locals.el, or file-local variables:
  # Local Variables:
  # feynman-chiron-database-url: \"postgresql://user:pass@host:port/chiron\"
  # End:

This is the base database. Schemas are specified separately.
Can be set once in ~/learning directory and shared across all org files.")
(put 'feynman-chiron-database-url 'safe-local-variable #'stringp)

(defvar-local feynman-chiron-embedding-model nil
  "Embedding model for `feynman-chiron-ingest-textbook' on THIS project.
A Hugging Face Hub model id, e.g. \"sentence-transformers/all-MiniLM-L6-v2\"
(the default when nil) or \"BAAI/bge-small-en-v1.5\". Set via file-local
variables or `.dir-locals.el', same as `feynman-chiron-textbook-sources':
  # Local Variables:
  # feynman-chiron-embedding-model: \"BAAI/bge-small-en-v1.5\"
  # End:

Only matters on a schema's FIRST ingest — chiron-ingest fixes the model a
schema uses at that point and rejects a later ingest with a different one
(`Storage::ensure_textbook_schema' in chiron-rs/core/src/storage.rs), so
different projects/schemas can use different models, but a given schema
cannot be switched between models in place.")
(put 'feynman-chiron-embedding-model 'safe-local-variable #'stringp)

(defvar-local feynman-chiron-learning-schema nil
  "PostgreSQL schema for learning state (graph, checkpoints, sessions).
Set via file-local variables or .dir-locals.el:
  # Local Variables:
  # feynman-chiron-learning-schema: \"learning\"
  # End:

This schema stores:
- Knowledge graph (concepts, mastery, relationships)
- Agent checkpoints
- Learning session history")
(put 'feynman-chiron-learning-schema 'safe-local-variable #'stringp)

(defvar-local feynman-chiron-textbook-sources nil
  "Alist of textbook sources for RAG.

Format 1 - Just schema name (uses base database):
  ((\"dummit-foote\" . \"math\") (\"lang\" . \"math\"))

Format 2 - Full database URL with schema:
  ((\"dummit-foote\" . (\"postgresql://other-server/chiron\" . \"math\")))

Format 3 - Mixed:
  ((\"dummit-foote\" . \"math\")
   (\"griffiths\" . (\"postgresql://physics-server/chiron\" . \"physics\")))

Set via file-local variables:
  # Local Variables:
  # feynman-chiron-textbook-sources: ((\"dummit-foote\" . \"math\")
  #   (\"lang\" . \"math\"))
  # End:

The agent queries all specified sources.")
(defun feynman-chiron--safe-textbook-sources-p (value)
  "Return non-nil if VALUE is a well-formed `feynman-chiron-textbook-sources'.
Each entry is (NAME . SCHEMA) or (NAME . (DATABASE-URL . SCHEMA)), all
strings — used as this variable's `safe-local-variable' predicate, so
opening a file that sets it never needs an interactive confirmation."
  (and (listp value)
       (seq-every-p
        (lambda (entry)
          (and (consp entry)
               (stringp (car entry))
               (let ((spec (cdr entry)))
                 (or (stringp spec)
                     (and (consp spec) (stringp (car spec)) (stringp (cdr spec)))))))
        value)))
(put 'feynman-chiron-textbook-sources 'safe-local-variable
     #'feynman-chiron--safe-textbook-sources-p)

(defcustom feynman-chiron-backend-buffer " *feynman-backend*"
  "Buffer name for the backend process's stdout.
chiron-rs's protocol is one newline-terminated JSON object per line
on stdout; this buffer must contain ONLY that (see
`feynman-chiron-backend-stderr-buffer' for its diagnostic output)."
  :type 'string
  :group 'feynman-chiron)

(defcustom feynman-chiron-backend-stderr-buffer " *feynman-backend-stderr*"
  "Buffer name for the backend process's stderr (progress/diagnostics).
Kept separate from `feynman-chiron-backend-buffer' — chiron-rs's
stdout is a line-based JSON protocol, and `make-process' merges
stderr into the same buffer as stdout unless given a distinct
destination, which would otherwise corrupt the JSON stream with
interleaved diagnostic text."
  :type 'string
  :group 'feynman-chiron)

(defconst feynman-chiron--github-repo "kamysh/feynman-chiron"
  "GitHub \"owner/repo\" slug the chiron-rs/chiron-ingest binaries are released from.")

(defconst feynman-chiron--binaries '("chiron-rs" "chiron-ingest")
  "Names of the Rust binaries this package drives as subprocesses.
Both are built from the same chiron-rs/ Nix flake output.")

(defcustom feynman-chiron-backend-install-dir
  (expand-file-name "bin/" feynman-chiron--package-dir)
  "Directory `feynman-chiron-install-backend' installs binaries into.
These binaries have no use outside this package, so they're kept
package-private here rather than added to your shell PATH;
`feynman-chiron--find-binary' auto-detects them from this directory
without any PATH or `feynman-chiron-backend-program' setup needed.

Defaults inside the package's own checkout directory rather than
somewhere shared like `user-emacs-directory' — that way the binaries
need no separate `.gitignore' entry (a `package-vc-install' checkout's
directory is already excluded wholesale by any sane dotfiles setup)
and are automatically cleaned up if the checkout itself is ever
deleted and recloned, rather than becoming orphaned files elsewhere."
  :type 'directory
  :group 'feynman-chiron)

(defun feynman-chiron--release-asset-name (binary-name)
  "Return the release asset name for BINARY-NAME on the current platform,
or nil if the platform isn't one we publish binaries for."
  (let ((arch (cond ((string-match-p "aarch64\\|arm64" system-configuration) "arm64")
                     ((string-match-p "x86_64" system-configuration) "amd64")
                     (t nil))))
    (when arch
      (pcase system-type
        ('gnu/linux (format "%s-linux-%s" binary-name arch))
        ('darwin    (format "%s-darwin-%s" binary-name arch))
        (_ nil)))))

(defun feynman-chiron--download-binary (binary-name)
  "Download the prebuilt BINARY-NAME binary for this platform.
Returns the installed path, or signals an error."
  (let ((asset (feynman-chiron--release-asset-name binary-name)))
    (unless asset
      (error "No prebuilt %s binary for this platform (%s/%s)"
             binary-name system-type system-configuration))
    (require 'url)
    (make-directory feynman-chiron-backend-install-dir t)
    (let ((dest (expand-file-name binary-name feynman-chiron-backend-install-dir))
          (url (format "https://github.com/%s/releases/latest/download/%s"
                       feynman-chiron--github-repo asset)))
      (message "Downloading %s..." url)
      (url-copy-file url dest t)
      (set-file-modes dest #o755)
      (message "Installed %s to %s" binary-name dest)
      dest)))

(defun feynman-chiron--install-binary (binary-name)
  "Download the prebuilt BINARY-NAME binary for this platform.
Returns the installed path. This is the only automatic install path —
building from source via Nix is a manual developer workflow (see the
README's \"local development\" section), never triggered automatically,
because a source tree happening to be present next to the package
(e.g. every `package-vc-install' checkout includes it) is not evidence
anyone wants to pay for a from-source build."
  (feynman-chiron--download-binary binary-name))

;;;###autoload
(defun feynman-chiron-install-backend ()
  "Install both the chiron-rs and chiron-ingest binaries.
See `feynman-chiron--install-binary' for how each is obtained.
Returns the installed path of chiron-rs, for backwards compatibility."
  (interactive)
  (let ((chiron-rs-path (feynman-chiron--install-binary "chiron-rs")))
    (feynman-chiron--install-binary "chiron-ingest")
    chiron-rs-path))

(defun feynman-chiron--package-version ()
  "Return this package's own version, from feynman-chiron.el's Version header."
  (let ((file (expand-file-name "feynman-chiron.el" feynman-chiron--package-dir)))
    (when (file-exists-p file)
      (with-temp-buffer
        (insert-file-contents file nil 0 4096)
        (lm-header "version")))))

(defun feynman-chiron--binary-version (binary-path)
  "Return BINARY-PATH's reported version via --version, or nil.
Expects output of the form \"chiron-rs 2.0.0\" — the last
whitespace-separated token is taken as the version."
  (with-temp-buffer
    (when (ignore-errors (zerop (call-process binary-path nil t nil "--version")))
      (car (last (split-string (string-trim (buffer-string))))))))

(defun feynman-chiron--ensure-fresh (binary-name path)
  "Return PATH, silently reinstalling BINARY-NAME first if it's stale.
\"Stale\" means PATH's --version disagrees with the package's own
Version header. No prompt — a user should never have to decide
whether to update a backend binary; a network failure here just
leaves the old (still usually usable) binary in place, reported via
`message', not an error."
  (let ((pkg-version (feynman-chiron--package-version))
        (bin-version (feynman-chiron--binary-version path)))
    (if (and pkg-version bin-version (not (equal pkg-version bin-version)))
        (condition-case err
            (feynman-chiron--install-binary binary-name)
          (error
           (message "feynman-chiron: %s is v%s (package is v%s) and reinstalling it failed: %s"
                     binary-name bin-version pkg-version (error-message-string err))
           path))
      path)))

(defun feynman-chiron--find-binary (binary-name)
  "Find the BINARY-NAME executable, installing it if necessary.
Looks on PATH, next to the package source, in
`feynman-chiron-backend-install-dir', then installs it there — all
silently, with no prompt; see `feynman-chiron--ensure-fresh' for how
a stale previously-installed binary is handled the same way. PATH and
`feynman-chiron-backend-program' are the user's own explicit choice
and are never second-guessed or reinstalled."
  (or (and (equal binary-name "chiron-rs") feynman-chiron-backend-program)
      ;; Look in PATH first
      (executable-find binary-name)
      ;; Then relative to the package directory (a `cargo build' dev tree)
      (let ((source-build (expand-file-name
                            (concat "chiron-rs/target/release/" binary-name)
                            feynman-chiron--package-dir)))
        (and (file-exists-p source-build) source-build))
      ;; Then a prior auto-install, kept up to date.
      (let ((installed (expand-file-name binary-name feynman-chiron-backend-install-dir)))
        (and (file-exists-p installed)
             (feynman-chiron--ensure-fresh binary-name installed)))
      ;; Not found anywhere: install it now, no prompt.
      (condition-case err
          (feynman-chiron--install-binary binary-name)
        (error
         (message "feynman-chiron: could not install %s: %s"
                   binary-name (error-message-string err))
         nil))))

(defun feynman-chiron--ensure-backend-installed ()
  "Silently install any missing chiron-rs/chiron-ingest binaries.
Run once, shortly after the package loads (see the `run-with-idle-timer'
call below) — a user should never have to invoke an install command or
answer a prompt just to use this package; by the time they run
`feynman-chiron-start' the backend is normally already there."
  (dolist (binary-name feynman-chiron--binaries)
    (feynman-chiron--find-binary binary-name)))

;;; Textbook ingestion (chiron-ingest)

(defconst feynman-chiron--ingest-buffer "*chiron-ingest*"
  "Buffer name for chiron-ingest's output.")

(defun feynman-chiron--run-ingest (&rest args)
  "Run chiron-ingest with ARGS; show output in `feynman-chiron--ingest-buffer'.
Returns t on success (exit 0), signals an error otherwise."
  (let ((binary (feynman-chiron--find-binary "chiron-ingest")))
    (unless binary
      (error "chiron-ingest binary not available"))
    (let ((buffer (get-buffer-create feynman-chiron--ingest-buffer)))
      (with-current-buffer buffer
        (let ((inhibit-read-only t)) (erase-buffer)))
      (display-buffer buffer)
      (let ((status (apply #'call-process binary nil buffer t args)))
        (unless (zerop status)
          (error "chiron-ingest failed (exit %d); see %s" status feynman-chiron--ingest-buffer))
        t))))

(defun feynman-chiron--require-database-url (database-url)
  "Return DATABASE-URL, or `feynman-chiron-database-url', or signal an error."
  (or database-url feynman-chiron-database-url
      (error "No database URL configured. Set feynman-chiron-database-url")))

(defun feynman-chiron--run-ingest-lines (&rest args)
  "Run chiron-ingest with ARGS, returning its stdout as a list of lines.
Unlike `feynman-chiron--run-ingest', this is for quiet queries
(list-schemas, list-textbooks): no buffer is shown, and a failure
returns nil rather than signaling — callers use this to populate
completion candidates and must degrade to free-text input when it's
unavailable (binary missing, database unreachable, etc.), not treat
that as fatal."
  (let ((binary (feynman-chiron--find-binary "chiron-ingest")))
    (when binary
      (with-temp-buffer
        (when (zerop (apply #'call-process binary nil t nil args))
          (split-string (buffer-string) "\n" t))))))

(defun feynman-chiron--read-schema (prompt database-url)
  "Read a schema name with PROMPT, completing against DATABASE-URL's schemas.
Falls back to plain `read-string' when the candidate list can't be
fetched — never blocks on the database being reachable right now.
Free text is always accepted; the returned schema need not already
exist (e.g. `feynman-chiron-create-schema' names a new one)."
  (let ((schemas (and database-url (feynman-chiron--run-ingest-lines "list-schemas" database-url))))
    (completing-read prompt schemas nil nil)))

(defun feynman-chiron--read-textbook-name (prompt database-url schema &optional require-match)
  "Read a textbook name with PROMPT, completing against SCHEMA's textbooks.
Same fallback behavior as `feynman-chiron--read-schema'. REQUIRE-MATCH
non-nil restricts to an existing textbook (appropriate for search;
ingest is naming a possibly-new one, so leave it nil there)."
  (let ((names (and database-url schema
                     (feynman-chiron--run-ingest-lines
                      "list-textbooks" "--schema" schema database-url))))
    (completing-read prompt names nil require-match)))

;;;###autoload
(defun feynman-chiron-create-schema (database-url schema)
  "Create SCHEMA on DATABASE-URL via chiron-ingest."
  (interactive
   (let ((db (read-string "Database URL: " feynman-chiron-database-url)))
     (list db (feynman-chiron--read-schema "Schema name: " db))))
  (feynman-chiron--run-ingest "create-schema" database-url schema)
  (message "Schema '%s' created (or already exists)" schema))

;;;###autoload
(defun feynman-chiron-ingest-textbook (pdf-path textbook-name schema &optional database-url model)
  "Ingest PDF-PATH as TEXTBOOK-NAME into SCHEMA via chiron-ingest.
Uses `feynman-chiron-database-url' unless DATABASE-URL is given.

MODEL is a Hugging Face Hub embedding model id, defaulting to
`feynman-chiron-embedding-model'; leave it empty to use chiron-ingest's
own default. It only takes effect on SCHEMA's first ingest — see
`feynman-chiron-embedding-model' for why."
  (interactive
   (let* ((pdf (read-file-name "PDF file: " nil nil t))
          (name (read-string "Textbook name: "))
          (db (feynman-chiron--require-database-url nil))
          (schema (feynman-chiron--read-schema "Schema: " db)))
     (list pdf name schema nil
           (read-string "Embedding model (empty for default): "
                        nil nil feynman-chiron-embedding-model))))
  (let ((db-url (feynman-chiron--require-database-url database-url))
        (model (or (and model (not (string-empty-p model)) model)
                   feynman-chiron-embedding-model)))
    (apply #'feynman-chiron--run-ingest
           "ingest" "--schema" schema
           (append (when model (list "--model" model))
                   (list db-url (expand-file-name pdf-path) textbook-name)))
    (message "Ingested '%s' into schema '%s'" textbook-name schema)))

;;;###autoload
(defun feynman-chiron-search-textbook (textbook-name query schema &optional database-url k)
  "Search TEXTBOOK-NAME in SCHEMA for QUERY via chiron-ingest.
Uses `feynman-chiron-database-url' unless DATABASE-URL is given.
Shows the top K (default 3) results in `feynman-chiron--ingest-buffer'."
  (interactive
   (let* ((db (feynman-chiron--require-database-url nil))
          (schema (feynman-chiron--read-schema "Schema: " db))
          (name (feynman-chiron--read-textbook-name "Textbook name: " db schema t)))
     (list name (read-string "Query: ") schema)))
  (let ((db-url (feynman-chiron--require-database-url database-url))
        (args (list "search" "--schema" schema)))
    (when k (setq args (append args (list "-k" (number-to-string k)))))
    (setq args (append args (list db-url textbook-name query)))
    (apply #'feynman-chiron--run-ingest args)))

(defun feynman-chiron--normalize-textbook-sources ()
  "Normalize textbook sources to format expected by backend.
Converts both formats to: {\"name\": {\"database\": \"url\", \"schema\": \"name\"}}
or {\"name\": {\"schema\": \"name\"}} for simple format."
  (let ((normalized '()))
    (dolist (source feynman-chiron-textbook-sources)
      (let* ((name (car source))
             (spec (cdr source))
             (entry nil))
        (cond
         ;; Simple format: ("name" . "schema")
         ((stringp spec)
          (setq entry (cons name (list (cons "schema" spec)))))

         ;; Complex format: ("name" . ("database-url" . "schema"))
         ((and (consp spec) (stringp (car spec)) (stringp (cdr spec)))
          (setq entry (cons name (list (cons "database" (car spec))
                                      (cons "schema" (cdr spec))))))

         (t
          (error "Invalid textbook source format for '%s'" name)))

        (push entry normalized)))
    (nreverse normalized)))

(defun feynman-chiron--start-backend ()
  "Start the chiron-rs agent backend."
  ;; Skip if backend already running
  (unless (and feynman-chiron-backend-process
               (process-live-p feynman-chiron-backend-process))

    (unless feynman-chiron-database-url
      (error "No database URL configured. Set feynman-chiron-database-url"))

    (unless feynman-chiron-learning-schema
      (error "No learning schema configured. Set feynman-chiron-learning-schema in file-local variables"))

    (let ((binary (feynman-chiron--find-binary "chiron-rs")))
    (unless binary
      (error "Cannot find chiron-rs binary. Run M-x feynman-chiron-install-backend"))

    (message "Starting Chiron agent: %s" binary)

    ;; Normalize textbook sources for backend
    (let* ((normalized-sources (feynman-chiron--normalize-textbook-sources))
           (provider (feynman-chiron--get-provider))
           ;; Resolve only the active provider's key: resolving the other
           ;; one too (via auth-source) can trigger an unwanted/unrelated
           ;; secret lookup (e.g. a GPG prompt) even when it's unused.
           (chiron-api-key (if (eq provider 'anthropic)
                                (feynman-chiron--anthropic-key)
                              (feynman-chiron--openai-key)))
           (process-environment
            (append
             (list
              (format "CHIRON_PROVIDER=%s" provider)
              (format "CHIRON_MODEL=%s" (feynman-chiron--model))
              (format "CHIRON_DATABASE_URL=%s" feynman-chiron-database-url)
              (format "CHIRON_LEARNING_SCHEMA=%s" feynman-chiron-learning-schema)
              (format "CHIRON_TEXTBOOK_SOURCES=%s"
                      (json-encode normalized-sources)))
             (when chiron-api-key
               (list (format "CHIRON_API_KEY=%s" chiron-api-key)))
             (when feynman-chiron-endpoint-url
               (list (format "CHIRON_ENDPOINT_URL=%s" feynman-chiron-endpoint-url)))
             process-environment)))

      (setq feynman-chiron-backend-process
            (make-process
             :name "chiron-agent"
             :buffer feynman-chiron-backend-buffer
             :stderr feynman-chiron-backend-stderr-buffer
             :command (list binary)
             :connection-type 'pipe
             :sentinel #'feynman-chiron--backend-sentinel))

      ;; Wait for READY signal with proper error handling
      (condition-case err
          (with-timeout (5 (signal 'timeout nil))
            (while (not (with-current-buffer feynman-chiron-backend-buffer
                         (goto-char (point-min))
                         (search-forward "READY" nil t)))
              (sleep-for 0.1)))
        (timeout
         ;; Cleanup on timeout
         (when (process-live-p feynman-chiron-backend-process)
           (kill-process feynman-chiron-backend-process))
         (setq feynman-chiron-backend-process nil)
         ;; Show backend error output
         (let ((backend-output (with-current-buffer feynman-chiron-backend-buffer
                                 (buffer-string))))
           (error "Backend startup timeout. Backend output:\n%s"
                  (if (string-empty-p backend-output)
                      "(no output)"
                    backend-output))))
        (error
         ;; Cleanup on other errors
         (when (and feynman-chiron-backend-process
                    (process-live-p feynman-chiron-backend-process))
           (kill-process feynman-chiron-backend-process))
         (setq feynman-chiron-backend-process nil)
         (signal (car err) (cdr err))))

      (message "Chiron agent ready"))))) ; Close unless block

(defun feynman-chiron--backend-sentinel (process event)
  "Handle backend process events."
  (unless (process-live-p process)
    (message "Feynman backend stopped: %s" event)))

(defcustom feynman-chiron-response-timeout 60
  "Seconds to wait for a `process' response from the chiron-rs agent.
A real LLM call (especially through a CLI-proxying endpoint like
Meridian, which spawns a fresh `claude' subprocess per request, or a
cold embedding-model load on the first retrieval) can legitimately
take well over 10 seconds — measured ~20s through Meridian during
testing. Too short a timeout here surfaces as a misleading
\"JSON parse error: (json-end-of-file)\", not a timeout message."
  :type 'integer
  :group 'feynman-chiron)

(defun feynman-chiron--call-backend (command-dict)
  "Call backend with COMMAND-DICT, return response."
  (unless (and feynman-chiron-backend-process
               (process-live-p feynman-chiron-backend-process))
    (feynman-chiron--start-backend))

  (let ((json-string (json-encode command-dict)))
    ;; Clear output buffer
    (with-current-buffer feynman-chiron-backend-buffer
      (erase-buffer))

    ;; Send command
    (process-send-string feynman-chiron-backend-process
                        (concat json-string "\n"))

    ;; Wait for a full response line. chiron-rs writes one newline-terminated
    ;; JSON object per response; waiting on buffer-size alone races against
    ;; partial pipe reads and can hand json-read a truncated object.
    (with-timeout (feynman-chiron-response-timeout nil)
      (while (not (with-current-buffer feynman-chiron-backend-buffer
                    (goto-char (point-min))
                    (search-forward "\n" nil t)))
        (sleep-for 0.05)))

    ;; Parse response
    (with-current-buffer feynman-chiron-backend-buffer
      (goto-char (point-min))
      (let ((json-object-type 'alist)
            (json-array-type 'list))
        (condition-case err
            (json-read)
          (error
           (list (cons 'success nil)
                 (cons 'error (format "JSON parse error: %s" err)))))))))

(defun feynman-chiron--backend-ready-p ()
  "Check if backend is ready and textbook sources are configured for
current buffer."
  (and feynman-chiron-textbook-sources
       (condition-case nil
           (progn
             (feynman-chiron--start-backend)
             t)
         (error nil))))

(defun feynman-chiron--process-with-agent (concept explanation thread-id)
  "Process EXPLANATION of CONCEPT through the chiron-rs agent.
THREAD-ID identifies the session for checkpointing — callers pass a
stable per-concept id (see `feynman-chiron--subtree-thread-id'), not
derived here, since this function has no reliable way to know which
org heading/file the caller is really working on.

Returns a LIST of turn strings — usually one, but `probe' splits its
2-3 questions into separate turns so each gets its own `Chiron:'/`You:'
pair (see `feynman-chiron--insert-response')."
  ;; chiron-rs expects textbook_sources as a JSON array of source NAMES
  ;; (Vec<String>), not the raw (name . spec) alist. Encode as a vector,
  ;; not a list: json-encode can't distinguish an empty list from nil/false
  ;; and would emit `null' instead of `[]', which chiron-rs rejects
  ;; ("invalid type: null, expected a sequence").
  (let ((textbook-sources (vconcat (mapcar #'car feynman-chiron-textbook-sources))))

    (condition-case err
        (let ((response (feynman-chiron--call-backend
                        `((command . "process")
                          (concept . ,concept)
                          (explanation . ,explanation)
                          (textbook_sources . ,textbook-sources)
                          (thread_id . ,thread-id)))))
          (if (alist-get 'success response)
              (or (alist-get 'turns response)
                  (let ((single (alist-get 'response response)))
                    (if (and single (not (string-empty-p single)))
                        (list single)
                      (progn
                        (message "Agent reported success but sent no response text: %S" response)
                        (list "(No response text from agent.)")))))
            (progn
              (message "Agent processing failed: %s" (alist-get 'error response))
              (list "Error processing your explanation. Please try again."))))
      (error
       (message "Agent error: %s" err)
       (list "Error communicating with agent.")))))

(defun feynman-chiron--stop-backend ()
  "Stop the backend process."
  (interactive)
  (when (and feynman-chiron-backend-process
             (process-live-p feynman-chiron-backend-process))
    (kill-process feynman-chiron-backend-process)
    (setq feynman-chiron-backend-process nil)
    (message "Backend stopped")))

;;; Subtree-based session identity and content

(defun feynman-chiron--subtree-thread-id ()
  "Get or create a stable org-id for the heading at point.
This is the real per-concept session identity: stable across
renames/reordering, unlike deriving anything from `buffer-file-name'
(which is wrong here anyway — see the fixed bug in
`feynman-chiron--process-with-agent')."
  (save-excursion
    (org-back-to-heading t)
    (org-id-get-create)))

(defun feynman-chiron--subtree-concept ()
  "Get the heading text at point, as the concept name."
  (save-excursion
    (org-back-to-heading t)
    (org-get-heading t t t t)))

(defun feynman-chiron--subtree-explanation ()
  "Get the current subtree's text, as the full `Chiron:'/`You:' transcript.
Sent whole on every submit, UNSTRIPPED — nothing here is a delta. Chiron
sees its own prior turns as context, which is what lets it avoid repeating
itself and notice when it already answered something (see agent.rs's
`count_chiron_turns'/`last_student_turn')."
  (save-excursion
    (org-back-to-heading t)
    ;; org-end-of-meta-data (not forward-line) so the heading's own
    ;; PROPERTIES drawer — org-id-get-create puts an :ID: drawer right
    ;; after the heading — is never mistaken for part of the
    ;; explanation and resubmitted to the LLM on every future turn.
    (let ((beg (progn (org-end-of-meta-data t) (point)))
          (end (progn (feynman-chiron--own-body-end) (point))))
      (string-trim (buffer-substring-no-properties beg end)))))

(defun feynman-chiron--own-body-end ()
  "Move point to the end of the CURRENT heading's own body text —
before its first child heading, if it has one. `org-end-of-subtree'
walks past child headings too (they're part of the subtree), which is
wrong here: a child heading is a different sub-concept, not more of
this heading's conversation, so its content must never be read as (or
have a response inserted into) this heading's explanation.
`outline-next-heading' stops at the next heading of ANY level — child
or sibling — which is exactly the boundary we want; it moves to
`point-max' if there is no next heading at all."
  (outline-next-heading))

(defun feynman-chiron--insert-response (turns)
  "Insert TURNS (a list of strings) as separate `Chiron: '/`You: ' pairs
at the end of the CURRENT HEADING's own body — before any child heading,
never inside one (see `feynman-chiron--own-body-end'). Usually one turn,
but `probe' splits its questions into several — each gets its own
`You: ' reply slot immediately after it, rather than bunching one
`You: ' after the whole batch.

Ordinary, editable org content — nothing here is read-only or gets
stripped before resubmission, so there's no wrong place to type your
answer. A blank line always follows the LAST `You: ' before whatever
comes next (a child or sibling heading, or nothing) — without it,
`You: ' would run directly into that heading's `*' line and break
org's heading recognition. Point is left right after the LAST `You: ',
ready to type."
  (let (after-last-you)
    (save-excursion
      (org-back-to-heading t)
      (feynman-chiron--own-body-end)
      (unless (bolp) (insert "\n"))
      (insert "\n")
      (dolist (turn turns)
        (insert "Chiron: " (string-trim turn) "\n\nYou: ")
        (setq after-last-you (point))
        (insert "\n\n")))
    (goto-char after-last-you)))

;;; Commands

;;;###autoload
(defun feynman-chiron-submit ()
  "Submit the org subtree at point to Chiron.

Sends the CURRENT full subtree text — the whole `Chiron:'/`You:'
transcript so far, unstripped — as context for the heading's concept.
Chiron's response is appended as one or more new `Chiron: ' turns, each
followed by its own `You: ' prompt; nothing is locked read-only, and
nothing is ever removed before resubmitting."
  (interactive)
  (unless (derived-mode-p 'org-mode)
    (user-error "feynman-chiron-submit only works in an org-mode buffer"))
  (unless feynman-chiron-textbook-sources
    (user-error "No feynman-chiron-textbook-sources configured for this buffer"))
  (let* ((concept (save-excursion (org-back-to-heading t) (feynman-chiron--subtree-concept)))
         (thread-id (save-excursion (org-back-to-heading t) (feynman-chiron--subtree-thread-id)))
         (explanation (save-excursion (org-back-to-heading t) (feynman-chiron--subtree-explanation))))
    (when (string-empty-p explanation)
      (user-error "Nothing to submit — write your explanation under this heading first"))
    (message "Chiron: processing your explanation for \"%s\"..." concept)
    (let ((turns (feynman-chiron--process-with-agent concept explanation thread-id)))
      ;; Deliberately NOT wrapped in save-excursion: --insert-response
      ;; (which finds the heading itself via org-back-to-heading) leaves
      ;; point right after the new block, which is where a reply belongs —
      ;; restoring the pre-submit point would put the cursor back
      ;; inside/above the old text, inviting the next reply to land in the
      ;; wrong place again.
      (feynman-chiron--insert-response turns)
      (message "Chiron: response inserted."))))

;;; Mode

(defvar feynman-chiron-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "C-c C-c") 'feynman-chiron-submit)
    (define-key map (kbd "C-c C-m") 'feynman-chiron-menu)
    map)
  "Keymap for `feynman-chiron-mode'.")

;;;###autoload (autoload 'feynman-chiron-menu "feynman-chiron" nil t)
(transient-define-prefix feynman-chiron-menu ()
  "Feynman Chiron command menu.
The entry point for everything this package does — invoke this
(`M-x feynman-chiron-menu', or `C-c C-m' with `feynman-chiron-mode'
enabled) instead of having to remember individual command names."
  ["Feynman Chiron"
   ["Session"
    ("s" "Enable in this buffer" feynman-chiron-start)
    ("g" "Submit heading at point" feynman-chiron-submit)]
   ["Textbooks"
    ("c" "Create schema" feynman-chiron-create-schema)
    ("i" "Ingest textbook (PDF)" feynman-chiron-ingest-textbook)
    ("t" "Search/test a textbook" feynman-chiron-search-textbook)]
   ["Backend"
    ("b" "Install/reinstall chiron-rs + chiron-ingest" feynman-chiron-install-backend)]])

(defface feynman-chiron-speaker-face
  '((t :inherit bold :slant italic))
  "Face for the `Chiron:'/`You:' speaker markers, to make turns easier
to tell apart at a glance."
  :group 'feynman-chiron)

(defconst feynman-chiron--speaker-font-lock-keywords
  '(("^\\(Chiron:\\|You:\\)" 1 'feynman-chiron-speaker-face))
  "Font-lock keyword making the `Chiron:'/`You:' markers visually
distinct (bold+italic by default, via `feynman-chiron-speaker-face').
Deliberately DISPLAY-ONLY, added via `font-lock-add-keywords' rather
than by inserting real org markup characters (`*bold*') into the
buffer: the literal text `Chiron:'/`You:' at the start of a line is
what chiron-rs's transcript parsing matches on (see agent.rs's
`count_chiron_turns'/`last_student_turn'/`student_agrees'), so the
buffer's actual characters must stay exactly `Chiron:'/`You:'.")

;;;###autoload
(define-minor-mode feynman-chiron-mode
  "Minor mode for learning with Feynman Chiron, on top of `org-mode'.

Enable in any org buffer with `feynman-chiron-textbook-sources' (and
the other feynman-chiron-* buffer-local variables) set — typically via
file-local variables or `.dir-locals.el'. One org heading is one
concept; its subtree is your explanation, freely editable like any
other org text. `C-c C-c' (`feynman-chiron-submit') sends the current
subtree and appends Chiron's response as a `Chiron: ' turn, followed by
a `You: ' prompt for your reply.

\\{feynman-chiron-mode-map}"
  :lighter " Chiron"
  :keymap feynman-chiron-mode-map
  (if feynman-chiron-mode
      (font-lock-add-keywords nil feynman-chiron--speaker-font-lock-keywords)
    (font-lock-remove-keywords nil feynman-chiron--speaker-font-lock-keywords))
  (when (fboundp 'font-lock-flush) (font-lock-flush)))

;;;###autoload
(defun feynman-chiron-start ()
  "Enable `feynman-chiron-mode' in the current buffer and ready the backend.
There is no separate session buffer — this just turns the minor mode
on here, in your own org file, where the dialog actually lives."
  (interactive)
  (unless (derived-mode-p 'org-mode)
    (user-error "feynman-chiron-start must be run in an org-mode buffer"))
  (feynman-chiron-mode 1)
  (if (and feynman-chiron-database-url feynman-chiron-learning-schema)
      (condition-case err
          (progn (feynman-chiron--start-backend) (message "Chiron agent ready"))
        (error (message "Backend unavailable: %s" err)))
    (message "feynman-chiron-mode enabled (set feynman-chiron-database-url and feynman-chiron-learning-schema to start the backend)")))

;; Ensure the backend is installed automatically, once, shortly after
;; Emacs is idle — not synchronously at load time (that would add a
;; network round-trip to every startup's load sequence) and not
;; unconditionally in noninteractive Emacs (byte-compilation, batch
;; tooling, tests all `require' this file without wanting a download).
(unless noninteractive
  (run-with-idle-timer 1 nil #'feynman-chiron--ensure-backend-installed))

(provide 'feynman-chiron)

;;; feynman-chiron.el ends here
