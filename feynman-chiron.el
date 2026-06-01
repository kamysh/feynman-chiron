;;; feynman-chiron.el --- Feynman Technique learning with AI -*- lexical-binding: t; -*-

;; Copyright (C) 2025

;; Author: Valentyn
;; Version: 1.0
;; Package-Requires: ((emacs "27.1"))
;; Keywords: learning, education, ai

;;; Commentary:

;; Feynman Chiron implements the Feynman Technique for active learning.
;; 
;; You explain concepts in your own words, Chiron identifies gaps,
;; asks probing questions, and helps you refine until you truly understand.
;;
;; Usage:
;;   M-x feynman-chiron-start
;;   
;; In the buffer:
;;   - Type after the > prompt
;;   - C-c C-c to submit
;;   - Chiron responds, buffer becomes read-only except next prompt
;;

;;; Code:

(require 'json)
(require 'url)

;;; Customization

(defgroup feynman-chiron nil
  "Feynman Technique learning with AI."
  :group 'applications
  :prefix "feynman-chiron-")


(defcustom feynman-chiron-default-provider 'anthropic
  "Default API provider: openai or anthropic.
Can be overridden per-buffer with feynman-chiron-provider."
  :type '(choice (const :tag "OpenAI" openai)
                 (const :tag "Anthropic" anthropic))
  :group 'feynman-chiron)

(defvar-local feynman-chiron-provider nil
  "API provider for this buffer: openai or anthropic.
If nil, uses feynman-chiron-default-provider.
Set via file-local variables:
  # Local Variables:
  # feynman-chiron-provider: openai
  # End:")

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

;;; Internal variables

(defvar feynman-chiron-buffer-name "*Feynman Chiron*"
  "Name of the Chiron learning buffer.")

(defvar-local feynman-chiron-backend-process nil
  "Python backend process for this buffer.")

(defvar-local feynman-chiron-state nil
  "Current learning state for this buffer.
Plist containing:
  :concept - current concept being learned
  :stage - current stage (initial, explain, probe, refine, complete)
  :explanations - list of student explanations
  :gaps - identified gaps
  :mastered - alist of (concept . data)")

(defvar-local feynman-chiron-prompt-marker nil
  "Marker for start of current prompt in this buffer.")

;;; API Communication

(defun feynman-chiron--get-provider ()
  "Get the API provider for current buffer."
  (or feynman-chiron-provider
      feynman-chiron-default-provider))

(defun feynman-chiron--api-key ()
  "Get API key for the current provider."
  (let* ((provider (feynman-chiron--get-provider))
         (key (if (eq provider 'openai)
                  (api-keys-get-openai)
                (api-keys-get-anthropic))))
    (or key
        (error "No API key set for %s. Check ~/.authinfo.gpg" provider))))

(defun feynman-chiron--model ()
  "Get model name for the current provider."
  (let ((provider (feynman-chiron--get-provider)))
    (or feynman-chiron-model
        (if (eq provider 'openai)
            feynman-chiron-openai-model
          feynman-chiron-anthropic-model))))

(defun feynman-chiron--call-openai (messages)
  "Call OpenAI API with MESSAGES."
  (let* ((url-request-method "POST")
         (url-request-extra-headers
          `(("Content-Type" . "application/json")
            ("Authorization" . ,(concat "Bearer " (feynman-chiron--api-key)))))
         (url-request-data
          (encode-coding-string
           (json-encode
            `((model . ,(feynman-chiron--model))
              (messages . ,messages)
              (temperature . 0.3)))
           'utf-8))
         (buffer (url-retrieve-synchronously
                  "https://api.openai.com/v1/chat/completions"
                  nil nil 30)))
    (if (not buffer)
        (error "OpenAI API request failed")
      (with-current-buffer buffer
        (goto-char (point-min))
        (re-search-forward "^$")
        (let* ((json-object-type 'alist)
               (json-array-type 'list)
               (response (json-read)))
          (kill-buffer)
          ;; Extract message content from OpenAI response
          (let* ((choices (alist-get 'choices response))
                 (first-choice (car choices))
                 (message (alist-get 'message first-choice))
                 (content (alist-get 'content message)))
            content))))))

(defun feynman-chiron--call-anthropic (messages)
  "Call Anthropic API with MESSAGES.
Converts OpenAI-style messages to Anthropic format."
  (let* ((system-msg nil)
         (converted-messages
          (mapcar
           (lambda (msg)
             (let ((role (alist-get 'role msg))
                   (content (alist-get 'content msg)))
               (cond
                ;; Extract system message separately
                ((string= role "system")
                 (setq system-msg content)
                 nil)
                ;; Convert assistant to assistant
                ((string= role "assistant")
                 `((role . "assistant")
                   (content . ,content)))
                ;; Convert user to user
                ((string= role "user")
                 `((role . "user")
                   (content . ,content)))
                (t
                 `((role . "user")
                   (content . ,content))))))
           messages))
         ;; Remove nil (system messages)
         (filtered-messages (delq nil converted-messages))
         (url-request-method "POST")
         (url-request-extra-headers
          `(("Content-Type" . "application/json")
            ("x-api-key" . ,(feynman-chiron--api-key))
            ("anthropic-version" . "2023-06-01")))
         (request-body
          `((model . ,(feynman-chiron--model))
            (max_tokens . 4096)
            (temperature . 0.3)
            (messages . ,filtered-messages)))
         ;; Add system message if present
         (request-body-with-system
          (if system-msg
              (append request-body `((system . ,system-msg)))
            request-body))
         (url-request-data
          (encode-coding-string
           (json-encode request-body-with-system)
           'utf-8))
         (buffer (url-retrieve-synchronously
                  "https://api.anthropic.com/v1/messages"
                  nil nil 30)))
    (if (not buffer)
        (error "Anthropic API request failed")
      (with-current-buffer buffer
        (goto-char (point-min))
        (re-search-forward "^$")
        (let* ((json-object-type 'alist)
               (json-array-type 'list)
               (response (json-read)))
          (kill-buffer)
          ;; Extract content from Anthropic response
          (let* ((content-blocks (alist-get 'content response))
                 (first-block (car content-blocks))
                 (text (alist-get 'text first-block)))
            text))))))

(defun feynman-chiron--call-api (messages)
  "Call configured API provider with MESSAGES.
MESSAGES should be in OpenAI format (will be converted if needed):
  ((role . \"system\") (content . \"...\"))
  ((role . \"user\") (content . \"...\"))
  ((role . \"assistant\") (content . \"...\"))"
  (condition-case err
      (if (eq (feynman-chiron--get-provider) 'openai)
          (feynman-chiron--call-openai messages)
        (feynman-chiron--call-anthropic messages))
    (error
     (message "API call failed: %s" err)
     (signal (car err) (cdr err)))))

;;; Rust Backend Integration

(defcustom feynman-chiron-backend-program nil
  "Path to the chiron-rs binary.
If nil, looks for 'chiron-rs' on PATH and in the package directory."
  :type '(choice (const :tag "Auto-detect" nil)
                 (file :tag "Path to binary"))
  :group 'feynman-chiron)

(defcustom feynman-chiron-endpoint-url nil
  "Base URL for OpenAI-compatible LLM endpoint.
Required when CHIRON_PROVIDER is 'openai-compat' (Groq, Mistral, Ollama, etc.).
Examples:
  Groq:   https://api.groq.com/openai
  Ollama: http://localhost:11434/v1
If nil, defaults to https://api.openai.com when provider is 'openai'."
  :type '(choice (const :tag "Default (OpenAI)" nil)
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
  # feynman-chiron-textbook-sources: ((\"dummit-foote\" . \"math\") (\"lang\" . \"math\"))
  # End:

The agent queries all specified sources.")

(defcustom feynman-chiron-backend-buffer " *feynman-backend*"
  "Buffer name for backend process output."
  :type 'string
  :group 'feynman-chiron)

(defun feynman-chiron--find-backend ()
  "Find the chiron-rs binary."
  (or feynman-chiron-backend-program
      ;; Look in PATH first
      (executable-find "chiron-rs")
      ;; Then relative to the package directory
      (let ((candidates (list
                         (when load-file-name
                           (expand-file-name
                            "chiron-rs/target/release/chiron-rs"
                            (file-name-directory load-file-name)))
                         (expand-file-name
                          "chiron-rs/target/release/chiron-rs"
                          user-emacs-directory))))
        (seq-find #'file-exists-p candidates))))

(defun feynman-chiron--build-db-url (schema)
  "Build database URL from base URL and SCHEMA name.
Uses feynman-chiron-database-url as base."
  (if (not feynman-chiron-database-url)
      (error "No database URL configured. Set feynman-chiron-database-url")
    (let ((base-url feynman-chiron-database-url))
      (format "%s?options=-c%%20search_path=%s" base-url schema))))

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
  "Start the LangGraph Chiron agent backend."
  ;; Skip if backend already running
  (unless (and feynman-chiron-backend-process
               (process-live-p feynman-chiron-backend-process))

    (unless feynman-chiron-database-url
      (error "No database URL configured. Set feynman-chiron-database-url"))

    (unless feynman-chiron-learning-schema
      (error "No learning schema configured. Set feynman-chiron-learning-schema in file-local variables"))

    (let ((binary (feynman-chiron--find-backend)))
    (unless binary
      (error "Cannot find chiron-rs binary. Build it with: cd chiron-rs && cargo build --release"))

    (message "Starting Chiron agent: %s" binary)

    ;; Normalize textbook sources for backend
    (let* ((normalized-sources (feynman-chiron--normalize-textbook-sources))
           ;; Get API keys from centralized api-keys.el (lazy-loaded)
           (openai-key (api-keys-get-openai))
           (anthropic-key (api-keys-get-anthropic))
           (provider (feynman-chiron--get-provider))
           ;; For openai-compat, use the openai key; for anthropic, the anthropic key
           (chiron-api-key (if (eq provider 'anthropic) anthropic-key openai-key))
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
             (when openai-key
               (list (format "OPENAI_API_KEY=%s" openai-key)))
             (when anthropic-key
               (list (format "ANTHROPIC_API_KEY=%s" anthropic-key)))
             (when feynman-chiron-endpoint-url
               (list (format "CHIRON_ENDPOINT_URL=%s" feynman-chiron-endpoint-url)))
             process-environment)))

      (setq feynman-chiron-backend-process
            (make-process
             :name "chiron-agent"
             :buffer feynman-chiron-backend-buffer
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
    
    ;; Wait for response
    (with-timeout (10 nil)
      (while (= 0 (buffer-size (get-buffer feynman-chiron-backend-buffer)))
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
  "Check if backend is ready and textbook sources are configured for current buffer."
  (and feynman-chiron-textbook-sources
       (condition-case nil
           (progn
             (feynman-chiron--start-backend)
             t)
         (error nil))))

(defun feynman-chiron--process-with-agent (concept explanation)
  "Process explanation through the LangGraph Chiron agent."
  (let ((textbook-sources (or feynman-chiron-textbook-sources '()))
        (thread-id (or (buffer-file-name) "default")))
    
    (condition-case err
        (let ((response (feynman-chiron--call-backend
                        `((command . "process")
                          (concept . ,concept)
                          (explanation . ,explanation)
                          (textbook_sources . ,textbook-sources)
                          (thread_id . ,thread-id)))))
          (if (alist-get 'success response)
              (alist-get 'response response)
            (progn
              (message "Agent processing failed: %s" (alist-get 'error response))
              "Error processing your explanation. Please try again.")))
      (error
       (message "Agent error: %s" err)
       "Error communicating with agent."))))

(defun feynman-chiron--stop-backend ()
  "Stop the backend process."
  (interactive)
  (when (and feynman-chiron-backend-process
             (process-live-p feynman-chiron-backend-process))
    (kill-process feynman-chiron-backend-process)
    (setq feynman-chiron-backend-process nil)
    (message "Backend stopped")))

;;; State Management

(defun feynman-chiron--init-state ()
  "Initialize learning state."
  (setq feynman-chiron-state
        (list :concept nil
              :stage 'waiting
              :explanations nil
              :gaps nil
              :mastered nil)))

(defun feynman-chiron--get-state (key)
  "Get value from state for KEY."
  (plist-get feynman-chiron-state key))

(defun feynman-chiron--set-state (key value)
  "Set VALUE in state for KEY."
  (setq feynman-chiron-state
        (plist-put feynman-chiron-state key value)))

(defun feynman-chiron--add-explanation (text)
  "Add explanation TEXT to history."
  (let ((explanations (feynman-chiron--get-state :explanations)))
    (feynman-chiron--set-state :explanations (append explanations (list text)))))

;;; Core Logic - All handled by backend agent

;;; Buffer Management

(defun feynman-chiron--insert-readonly (text &optional face)
  "Insert TEXT as read-only with optional FACE."
  (let ((start (point)))
    (insert text)
    (add-text-properties start (point)
                        '(read-only t front-sticky t rear-nonsticky t))
    (when face
      (add-text-properties start (point) (list 'face face)))))

(defun feynman-chiron--insert-prompt ()
  "Insert new prompt marker."
  (let ((inhibit-read-only t))
    (goto-char (point-max))
    (unless (bolp) (insert "\n"))
    (feynman-chiron--insert-readonly "\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n" 'shadow)
    (feynman-chiron--insert-readonly "YOU:\n" '(:foreground "cyan" :weight bold))
    (feynman-chiron--insert-readonly "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\n" 'shadow)
    (setq feynman-chiron-prompt-marker (point-marker))
    (set-marker-insertion-type feynman-chiron-prompt-marker nil)))

(defun feynman-chiron--get-prompt-text ()
  "Get text entered at current prompt."
  (when feynman-chiron-prompt-marker
    (buffer-substring-no-properties
     feynman-chiron-prompt-marker
     (point-max))))

(defun feynman-chiron--clear-prompt ()
  "Clear current prompt text."
  (when feynman-chiron-prompt-marker
    (let ((inhibit-read-only t))
      (delete-region feynman-chiron-prompt-marker (point-max)))))

(defun feynman-chiron--respond (text &optional face)
  "Insert Chiron's response TEXT."
  (let ((inhibit-read-only t))
    (goto-char (point-max))
    (unless (bolp) (insert "\n"))
    (feynman-chiron--insert-readonly "\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n" 'shadow)
    (feynman-chiron--insert-readonly "CHIRON:\n" '(:foreground "green" :weight bold))
    (feynman-chiron--insert-readonly "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\n" 'shadow)
    (let ((start (point)))
      (insert text)
      (add-text-properties start (point)
                          '(read-only t front-sticky t rear-nonsticky t))
      (when face
        (add-text-properties start (point) (list 'face face)))
      ;; Add light background to Chiron's text
      (add-text-properties start (point) '(face (:background "#1a1a1a"))))
    (feynman-chiron--insert-readonly "\n\n")))

;;; Commands

(defun feynman-chiron-submit ()
  "Submit current prompt to Chiron."
  (interactive)
  (let ((input (string-trim (feynman-chiron--get-prompt-text))))
    (when (string-empty-p input)
      (user-error "Empty input"))
    
    (feynman-chiron--clear-prompt)
    
    ;; Show what user submitted with visual distinction
    (let ((inhibit-read-only t))
      (goto-char (point-max))
      (let ((start (point)))
        (insert input "\n")
        (add-text-properties start (point)
                            '(read-only t front-sticky t rear-nonsticky t face (:foreground "white")))))
    
    (feynman-chiron--process-input input)
    (feynman-chiron--insert-prompt)))

(defun feynman-chiron--process-input (input)
  "Process user INPUT through the backend agent."
  (let* ((lines (split-string input "\n" t))
         (first-line (car lines))
         (parsed-concept nil)
         (explanation input))

    ;; Try to extract concept from first line
    (when (string-match "learning about \\(.+\\)[.!]?" first-line)
      (setq parsed-concept (match-string 1 first-line))
      ;; Remove the "I'm learning about X" line from explanation
      (setq explanation (string-join (cdr lines) "\n")))

    (let ((concept (or parsed-concept (feynman-chiron--get-state :concept))))

      ;; If still no concept, prompt for it
      (if (null concept)
          (feynman-chiron--respond
           "What concept are you learning?\nStart with: 'I'm learning about [concept name]'")

        ;; We have a concept - process through backend
        (feynman-chiron--set-state :concept concept)
        (feynman-chiron--add-explanation explanation)

        (feynman-chiron--respond "Processing your explanation...")

        (let ((response (feynman-chiron--process-with-agent concept explanation)))
          (feynman-chiron--respond response))))))

(defun feynman-chiron-show-progress ()
  "Show learning progress."
  (interactive)
  (let ((mastered (feynman-chiron--get-state :mastered)))
    (if (null mastered)
        (message "No concepts mastered yet")
      (with-current-buffer (get-buffer-create "*Feynman Progress*")
        (let ((inhibit-read-only t))
          (erase-buffer)
          (insert "=== Feynman Chiron Progress ===\n\n")
          (dolist (item mastered)
            (insert (format "✅ %s (Score: %d/10)\n"
                          (car item)
                          (plist-get (cdr item) :score)))
            (insert (format "   %s\n\n"
                          (substring (plist-get (cdr item) :explanation) 0
                                   (min 100 (length (plist-get (cdr item) :explanation)))))))
          (special-mode))
        (display-buffer (current-buffer))))))

(defun feynman-chiron-reset ()
  "Reset learning session."
  (interactive)
  (when (y-or-n-p "Reset all progress? ")
    (feynman-chiron--init-state)
    (let ((inhibit-read-only t))
      (erase-buffer)
      (feynman-chiron--insert-readonly "=== Feynman Chiron ===\n\n" 'bold)
      (feynman-chiron--insert-readonly "Learn using the Feynman Technique.\n")
      (feynman-chiron--insert-readonly
       "Write freely about a concept you're learning.
Start with: \"I'm learning about [concept]\"
Then explain it in your own words.\n\n")
      (feynman-chiron--insert-readonly "Write your explanation:\n")
      (feynman-chiron--insert-prompt))
    (message "Session reset")))

;;; Major Mode

(defvar feynman-chiron-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "C-c C-c") 'feynman-chiron-submit)
    (define-key map (kbd "C-c C-p") 'feynman-chiron-show-progress)
    (define-key map (kbd "C-c C-r") 'feynman-chiron-reset)
    map)
  "Keymap for Feynman Chiron mode.")

(define-derived-mode feynman-chiron-mode org-mode "Feynman-Chiron"
  "Major mode for learning with Feynman Chiron.
Based on org-mode for structured writing.

\\{feynman-chiron-mode-map}"
  (setq-local buffer-read-only t)
  ;; Enable org features
  (org-indent-mode 1)
  (visual-line-mode 1))

;;;###autoload
(defun feynman-chiron-start ()
  "Start Feynman Chiron learning session."
  (interactive)
  
  ;; Start backend if database and learning schema configured
  (when (and feynman-chiron-database-url feynman-chiron-learning-schema)
    (condition-case err
        (progn
          (feynman-chiron--start-backend)
          (message "Agent started"))
      (error
       (message "Backend unavailable: %s" err))))
  
  ;; Create or switch to buffer
  (let ((buffer (get-buffer-create feynman-chiron-buffer-name)))
    (with-current-buffer buffer
      (feynman-chiron-mode)
      
      (let ((inhibit-read-only t))
        (erase-buffer)
        
        (feynman-chiron--insert-readonly "=== Feynman Chiron ===\n\n" 'bold)
        
        ;; Show provider and model
        (feynman-chiron--insert-readonly
         (format "Provider: %s (%s)\n"
                (feynman-chiron--get-provider)
                (feynman-chiron--model))
         '(:foreground "cyan"))
        
        ;; Show database configuration
        (if feynman-chiron-database-url
            (let ((db-name (car (last (split-string feynman-chiron-database-url "/")))))
              (feynman-chiron--insert-readonly
               (format "Database: %s" db-name)
               '(:foreground "yellow"))
              (if feynman-chiron-learning-schema
                  (feynman-chiron--insert-readonly
                   (format " / %s\n" feynman-chiron-learning-schema)
                   '(:foreground "green"))
                (feynman-chiron--insert-readonly
                 " (no learning schema set)\n"
                 '(:foreground "red"))))
          (feynman-chiron--insert-readonly
           "Database: none (configure feynman-chiron-database-url)\n"
           '(:foreground "red")))
        
        ;; Show textbook sources
        (if feynman-chiron-textbook-sources
            (progn
              (feynman-chiron--insert-readonly
               (format "Textbooks: %d source(s)\n\n"
                      (length feynman-chiron-textbook-sources))
               '(:foreground "green")))
          (feynman-chiron--insert-readonly
           "Textbooks: none\n\n"
           '(:foreground "gray")))
        
        (feynman-chiron--insert-readonly
         "Learn using the Feynman Technique.

Write freely about a concept you're learning.
Start with: \"I'm learning about [concept]\"
Then explain it in your own words, as simply as possible.

When done, press C-c C-c
I'll identify gaps and help you refine.

Commands:
  C-c C-c  - Submit your explanation
  C-c C-p  - Show progress  
  C-c C-r  - Reset session\n\n")
        
        (when feynman-chiron-textbook-sources
          (feynman-chiron--insert-readonly "✓ Textbook sources:\n" '(:foreground "green"))
          (dolist (source feynman-chiron-textbook-sources)
            (feynman-chiron--insert-readonly
             (format "  - %s\n" (car source))
             '(:foreground "green")))
          (feynman-chiron--insert-readonly "\n"))
        
        (feynman-chiron--insert-readonly "Write your explanation:\n")
        (feynman-chiron--insert-prompt))
      
      (feynman-chiron--init-state))
    
    (switch-to-buffer buffer)
    (goto-char (point-max))))

(provide 'feynman-chiron)

;;; feynman-chiron.el ends here
