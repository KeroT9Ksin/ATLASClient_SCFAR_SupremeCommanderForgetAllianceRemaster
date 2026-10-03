use std::cell::RefCell;

thread_local! {
    static CUR_FILE: RefCell<Option<String>> = RefCell::new(None);
    static TRACE_FILE: RefCell<Option<String>> = RefCell::new(None);
}

/// Имя текущего файла для диагностики лексера ([LEXER]-лог).
pub fn set_lex_context(name: &str) {
    CUR_FILE.with(|s| *s.borrow_mut() = Some(name.to_string()));
}

/// Включить токен-трассировку стека (пустая строка — выключить).
/// Используется командой `lextrace <путь>`: печатает каждый push/pop с
/// номером строки, чтобы поймать рассинхрон, из-за которого ::__cont_N::
/// вставляется не перед end цикла (build_templates.lua:41).
pub fn set_lex_trace(name: &str) {
    TRACE_FILE.with(|s| {
        *s.borrow_mut() = if name.is_empty() { None } else { Some(name.to_lowercase()) };
    });
}

#[inline]
fn tr(line: usize, msg: &str) {
    TRACE_FILE.with(|s| {
        if let Some(f) = &*s.borrow() {
            eprintln!("[TRACE {} L{:04}] {}", f, line, msg);
        }
    });
}

// Moho-Lua -> Lua 5.1 (LuaJIT).
// Режимы: 0=goto/метки (канонический), 2=aggressive, 3=allcomment,
//         4=repeat/break, 5=nocontinue (continue остаётся словом, меток нет).
//
// ВАЖНО ПРО «#»: в Moho-диалекте «#» — ВСЕГДА комментарий до конца строки
// (доказано логом: #splash, #lowest layer orange, #LOG, ####, #-). Оператор
// длины «#t» в Moho не используется: таблицы обходятся через «for k,v in T do».
// Поэтому в режиме 0/3/4/5 «#» безусловно -> «--». is_length_pos — только в
// аварийном aggressive-режиме 2.

pub fn moho_to_lua51(src: &str) -> String {
    transform(src, 0)
}
pub fn moho_to_lua51_aggressive(src: &str) -> String {
    transform(src, 2)
}
pub fn moho_to_lua51_allcomment(src: &str) -> String {
    transform(src, 3)
}
pub fn moho_to_lua51_repeat(src: &str) -> String {
    transform(src, 4)
}
// РЕЖИМ 5: консервативный. continue НЕ преобразуется (остаётся словом),
// ни goto, ни меток, ни repeat-обёрток. LuaJIT даст синтаксическую ошибку
// near 'continue' -> UI-файл честно классифицируется как UiPending, а не
// как RealError с фантомной меткой ::__cont_N::.
pub fn moho_to_lua51_nocontinue(src: &str) -> String {
    transform(src, 5)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Blk {
    LoopPending,
    Loop(usize),
    Other,
    Fun, // вложенная function: goto/break через её границу нелегален в Lua
}

fn transform(src: &str, mode: u8) -> String {
    let b: Vec<char> = src.chars().collect();
    let n = b.len();
    let use_repeat = mode == 4;
    let keep_continue = mode == 5;
    let mut out = String::with_capacity(src.len() + 256);
    let mut inserts: Vec<(usize, String)> = Vec::new();
    let mut i = 0;
    let mut prev: char = '\0';
    let mut prev_word = String::new();
    let mut in_string: Option<char> = None;
    let mut cont_fallback = false;
    let mut for_has_eq = false;
    let mut for_in_pending = false;
    let mut in_open_pos = 0usize;
    let mut line_no: usize = 1;

    let mut stack: Vec<Blk> = Vec::new();
    let mut needs: Vec<bool> = Vec::new();
    let mut loop_counter: usize = 0;

    while i < n {
        let c = b[i];

        // ---------- внутри строки ----------
        if let Some(q) = in_string {
            // ГИГИЕНА ЛЕКСЕРА: в Lua сырой перевод строки внутри '…'/"…" незаконен.
            // Раньше строка «текла» до конца файла и съедала end/for/function.
            // Теперь обрыв на \n + лог. Для валидных файлов условие недостижимо
            // -> поведение побитово идентично.
            if c == '\n' {
                CUR_FILE.with(|s| {
                    if let Some(f) = &*s.borrow() {
                        eprintln!(
                            "[LEXER] незакрытая строка {}:{} (ожидалась {}), принудительное закрытие",
                            f, line_no, q
                        );
                    }
                });
                out.push('\n');
                in_string = None;
                i += 1;
                line_no += 1;
                prev = '\n';
                prev_word.clear();
                continue;
            }
            if c == '\\' {
                let nx = b.get(i + 1).copied();
                if nx == Some('\n') {
                    out.push('\\');
                    out.push('\n');
                    i += 2;
                    line_no += 1;
                    continue;
                }
                let keep = match nx {
                    Some('a') | Some('b') | Some('f') | Some('n') | Some('r')
                    | Some('t') | Some('v') | Some('\\') | Some('"') | Some('\'')
                    | Some('z') | Some('x') => true,
                    Some(ch) if ch.is_ascii_digit() => true,
                    _ => false,
                };
                if keep {
                    out.push('\\');
                }
                if let Some(x) = nx {
                    out.push(x);
                }
                i += 2;
                continue;
            }
            out.push(c);
            if c == q {
                in_string = None;
            }
            i += 1;
            continue;
        }

        match c {
            '\'' | '"' => {
                in_string = Some(c);
                out.push(c);
                i += 1;
            }
            '\n' => {
                out.push('\n');
                i += 1;
                line_no += 1;
                prev = '\n';
                prev_word.clear();
            }
            '[' => {
                let (eq, olen) = long_open(&b, i);
                if olen > 0 {
                    copy_until_close(&b, &mut i, &mut out, eq, olen, &mut line_no);
                    prev = '[';
                    prev_word.clear();
                } else {
                    out.push(c);
                    i += 1;
                    prev = c;
                    prev_word.clear();
                }
            }
            '-' if i + 1 < n && b[i + 1] == '-' => {
                if i + 3 < n && b[i + 2] == '[' && (b[i + 3] == '[' || b[i + 3] == '=') {
                    let (eq, olen) = long_open(&b, i + 2);
                    out.push_str(&seg(&b, i, 2 + olen));
                    i += 2 + olen;
                    while i < n {
                        if b[i] == '\n' {
                            line_no += 1;
                        }
                        if b[i] == ']' && closes(&b, i, eq) {
                            let cl = eq + 2;
                            out.push_str(&seg(&b, i, cl));
                            i += cl;
                            break;
                        }
                        out.push(b[i]);
                        i += 1;
                    }
                } else {
                    while i < n && b[i] != '\n' {
                        out.push(b[i]);
                        i += 1;
                    }
                }
                prev = '-';
                prev_word.clear();
            }
            '#' => {
                // В Moho «#» — всегда комментарий (см. шапку файла).
                let is_len = match mode {
                    0 | 3 | 4 | 5 => false,
                    _ => is_length_pos(prev, &prev_word, b.get(i + 1).copied()),
                };
                if is_len {
                    out.push(c);
                    i += 1;
                    prev = '#';
                    prev_word.clear();
                } else {
                    out.push_str("--");
                    i += 1;
                    while i < n && b[i] != '\n' {
                        out.push(b[i]);
                        i += 1;
                    }
                    prev = '-';
                    prev_word.clear();
                }
            }
            '&' | '|' => {
                let fname = if c == '&' { "bit.band(" } else { "bit.bor(" };
                let ls = scan_left(&out);
                let left = out[ls..].trim().to_string();
                let mut j = i + 1;
                while j < n && (b[j] == ' ' || b[j] == '\t' || b[j] == '\n' || b[j] == '\r') {
                    if b[j] == '\n' {
                        line_no += 1;
                    }
                    j += 1;
                }
                let re = scan_right(&b, j);
                let right: String = b[j..re].iter().collect();
                out.truncate(ls);
                match (!left.is_empty(), !right.is_empty()) {
                    (true, true) => {
                        out.push_str(fname);
                        out.push_str(&left);
                        out.push(',');
                        out.push_str(&right);
                        out.push(')');
                        i = re;
                    }
                    (true, false) => {
                        out.push_str(fname);
                        out.push_str(&left);
                        out.push_str(",0)");
                        i += 1;
                    }
                    (false, true) => {
                        out.push_str(fname);
                        out.push_str("0,");
                        out.push_str(&right);
                        out.push(')');
                        i = re;
                    }
                    (false, false) => {
                        out.push('0');
                        i += 1;
                    }
                }
                prev = ')';
                prev_word.clear();
            }
            '!' if i + 1 < n && b[i + 1] == '=' => {
                out.push_str("~=");
                i += 2;
                prev = '=';
                prev_word.clear();
            }
            ' ' | '\t' | '\r' => {
                out.push(c);
                i += 1;
            }
            _ if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < n && (b[i].is_alphanumeric() || b[i] == '_') {
                    i += 1;
                }
                let w: String = b[start..i].iter().collect();
                let next = b.get(i).copied();
                let is_ident_next = next.map(|d| d.is_alphanumeric() || d == '_').unwrap_or(false);
                match w.as_str() {
                    "for" | "while" => {
                        for_has_eq = false;
                        stack.push(Blk::LoopPending);
                        tr(line_no, &format!("PUSH LoopPending ({})", w));
                        out.push_str(&w);
                    }
                    // Нативный repeat учитывается в стеке ВО ВСЕХ режимах.
                    "repeat" => {
                        needs.push(false);
                        stack.push(Blk::Loop(loop_counter));
                        tr(line_no, &format!("PUSH Loop({}) repeat", loop_counter));
                        loop_counter += 1;
                        out.push_str(&w);
                    }
                    "in" => {
                        out.push_str(&w);
                        if let Some(Blk::LoopPending) = stack.last() {
                            if !for_has_eq {
                                for_in_pending = true;
                                in_open_pos = out.len();
                            }
                        }
                    }
                    "do" => {
                        if for_in_pending {
                            inserts.push((in_open_pos, " __moho_iter(".to_string()));
                            inserts.push((out.len(), ")".to_string()));
                            for_in_pending = false;
                        }
                        if let Some(Blk::LoopPending) = stack.last() {
                            needs.push(false);
                            *stack.last_mut().unwrap() = Blk::Loop(loop_counter);
                            tr(line_no, &format!("PROMOTE -> Loop({})", loop_counter));
                            loop_counter += 1;
                        } else {
                            stack.push(Blk::Other);
                            tr(line_no, "PUSH Other (bare do)");
                        }
                        out.push_str(&w);
                        if use_repeat {
                            if let Some(Blk::Loop(_)) = stack.last() {
                                inserts.push((out.len(), " repeat".to_string()));
                            }
                        }
                    }
                    "if" => {
                        stack.push(Blk::Other);
                        tr(line_no, "PUSH Other (if)");
                        out.push_str(&w);
                    }
                    // function — граница для поиска цикла при continue.
                    "function" => {
                        stack.push(Blk::Fun);
                        tr(line_no, "PUSH Fun");
                        out.push_str(&w);
                    }
                    "then" | "else" | "elseif" => {
                        out.push_str(&w);
                    }
                    "end" | "until" => {
                        for_in_pending = false;
                        for_has_eq = false;
                        if use_repeat && w == "end" {
                            let loop_end = matches!(stack.last(), Some(Blk::Loop(_)));
                            let expr_end = matches!(
                                next_nonspace(&b, i),
                                Some(')') | Some(',') | Some('.') | Some('(')
                            );
                            if loop_end && !expr_end {
                                inserts.push((out.len(), " until true".to_string()));
                            }
                        }
                        if !use_repeat && !keep_continue && cont_fallback {
                            inserts.push((out.len(), "\n ::__cont_fb::\n".to_string()));
                            cont_fallback = false;
                        }
                        match stack.pop() {
                            Some(Blk::Loop(id)) => {
                                tr(line_no, &format!("POP Loop({}) needs={}", id, needs[id]));
                                if !use_repeat && !keep_continue && needs[id] {
                                    inserts.push((out.len(), format!("\n ::__cont_{}::\n", id)));
                                }
                            }
                            Some(Blk::Fun) => tr(line_no, "POP Fun"),
                            Some(Blk::Other) => tr(line_no, "POP Other"),
                            Some(Blk::LoopPending) => {
                                tr(line_no, "POP LoopPending (!!! do не найден)")
                            }
                            None => tr(line_no, "POP на ПУСТОМ стеке (!!!!)"),
                        }
                        out.push_str(&w);
                    }
                    "continue" if !is_ident_next => {
                        if keep_continue {
                            // РЕЖИМ 5: оставляем слово как есть. Ни goto, ни меток,
                            // ни repeat-обёрток. LuaJIT упадёт с near 'continue' ->
                            // UI-файл честно классифицируется как UiPending.
                            tr(line_no, "continue -> KEEP (режим 5)");
                            out.push_str(&w);
                        } else if use_repeat {
                            let mut found = false;
                            for blk in stack.iter().rev() {
                                match blk {
                                    Blk::Loop(_) => {
                                        found = true;
                                        break;
                                    }
                                    Blk::Fun => break,
                                    _ => {}
                                }
                            }
                            if found {
                                tr(line_no, "continue -> break (режим 4)");
                                out.push_str("break");
                            } else {
                                tr(line_no, "continue -> KEEP (нет цикла до Fun/дна)");
                                out.push_str(&w);
                            }
                        } else {
                            let mut found = None;
                            let mut in_fun = false;
                            for blk in stack.iter().rev() {
                                match blk {
                                    Blk::Loop(id) => {
                                        found = Some(*id);
                                        break;
                                    }
                                    Blk::Fun => {
                                        in_fun = true;
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                            match found {
                                Some(id) => {
                                    needs[id] = true;
                                    tr(
                                        line_no,
                                        &format!(
                                            "continue -> goto __cont_{} (стек: {:?})",
                                            id, stack
                                        ),
                                    );
                                    out.push_str(&format!("goto __cont_{}", id));
                                }
                                // continue внутри вложенной function: межфункциональный
                                // goto нелегален. Оставляем слово как есть.
                                None if in_fun => {
                                    tr(line_no, "continue -> KEEP (внутри Fun)");
                                    out.push_str(&w);
                                }
                                None => {
                                    tr(line_no, "continue -> goto __cont_fb (FALLBACK)");
                                    cont_fallback = true;
                                    out.push_str("goto __cont_fb");
                                }
                            }
                        }
                    }
                    _ => {
                        out.push_str(&w);
                    }
                }
                prev_word = w.clone();
                prev = w.chars().last().unwrap_or('\0');
            }
            _ if c.is_ascii_digit() => {
                let start = i;
                while i < n
                    && (b[i].is_ascii_digit()
                        || b[i] == '.'
                        || b[i] == 'x'
                        || b[i] == 'X'
                        || ('a'..='f').contains(&b[i])
                        || ('A'..='F').contains(&b[i]))
                {
                    i += 1;
                }
                let num_str: String = b[start..i].iter().collect();
                out.push_str(&num_str);
                if i < n && b[i].is_alphabetic() {
                    let ks = i;
                    while i < n && (b[i].is_alphanumeric() || b[i] == '_') {
                        i += 1;
                    }
                    let kw: String = b[ks..i].iter().collect();
                    if is_keyword(&kw) {
                        out.push(' ');
                    }
                    out.push_str(&kw);
                    prev_word = kw.clone();
                    prev = kw.chars().last().unwrap_or('\0');
                } else {
                    prev = num_str.chars().last().unwrap_or('\0');
                    prev_word.clear();
                }
            }
            _ => {
                if c == '=' {
                    if let Some(Blk::LoopPending) = stack.last() {
                        for_has_eq = true;
                    }
                }
                out.push(c);
                i += 1;
                prev_word.clear();
                prev = c;
            }
        }
    }

    inserts.sort_by_key(|(p, _)| *p);
    for (p, s) in inserts.into_iter().rev() {
        out.insert_str(p, &s);
    }
    sanitize_bitops(&out)
}

// ==================== SELF-CHECK БАЛАНСА + ДАТЧИК ====================
// Инварианты валидного Lua:
//     end   ==  do + if + function
//     until ==  repeat
// FIX ДАТЧИКА: раньше блок --[[ ... ]] / --[==[ ... ]==] не распознавался, и
// английский текст LOC-блоков парсился как код (слова if/do/end), давая ложную
// ДЕЛЬТУ -3 в orders.lua (BALANCE(raw)==BALANCE(t0)). Теперь долгий комментарий
// пропускается целиком.
pub fn balance_report(src: &str) -> Option<String> {
    let b: Vec<char> = src.chars().collect();
    let n = b.len();
    let mut i = 0usize;
    let mut line: usize = 1;
    let mut n_do: i64 = 0;
    let mut n_if: i64 = 0;
    let mut n_fun: i64 = 0;
    let mut n_end: i64 = 0;
    let mut n_rep: i64 = 0;
    let mut n_unl: i64 = 0;
    let mut regions: Vec<(usize, &'static str, String, usize)> = Vec::new();

    while i < n {
        let c = b[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if c == '-' && i + 1 < n && b[i + 1] == '-' {
            // Долгий комментарий --[[ ... ]] / --[==[ ... ]==]
            if i + 3 < n && b[i + 2] == '[' && (b[i + 3] == '[' || b[i + 3] == '=') {
                let (eq, olen) = long_open(&b, i + 2);
                i += 2 + olen;
                while i < n {
                    if b[i] == '\n' {
                        line += 1;
                    }
                    if b[i] == ']' && closes(&b, i, eq) {
                        i += eq + 2;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            while i < n && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '#' {
            while i < n && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '[' {
            let (eq, olen) = long_open(&b, i);
            if olen > 0 {
                let start_line = line;
                let snip: String = b[i..std::cmp::min(i + olen + 24, n)].iter().collect();
                i += olen;
                while i < n {
                    if b[i] == '\n' {
                        line += 1;
                    }
                    if b[i] == ']' && closes(&b, i, eq) {
                        i += eq + 2;
                        break;
                    }
                    i += 1;
                }
                regions.push((start_line, "longbr", snip, line));
                continue;
            }
        }
        if c == '\'' || c == '"' {
            let q = c;
            let start_line = line;
            let snip: String = b[i..std::cmp::min(i + 24, n)].iter().collect();
            i += 1;
            while i < n {
                if b[i] == '\\' {
                    if i + 1 < n && b[i + 1] == '\n' {
                        line += 1;
                    }
                    i += 2;
                    continue;
                }
                if b[i] == q {
                    i += 1;
                    break;
                }
                if b[i] == '\n' {
                    line += 1;
                    break;
                }
                i += 1;
            }
            regions.push((start_line, "str", snip, line));
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < n && (b[i].is_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            let word: String = b[start..i].iter().collect();
            match word.as_str() {
                "do" => n_do += 1,
                "if" => n_if += 1,
                "function" => n_fun += 1,
                "end" => n_end += 1,
                "repeat" => n_rep += 1,
                "until" => n_unl += 1,
                _ => {}
            }
            continue;
        }
        i += 1;
    }

    let need_end = n_do + n_if + n_fun;
    let d_end = n_end - need_end;
    let d_unl = n_unl - n_rep;
    if d_end == 0 && d_unl == 0 {
        return None;
    }
    let mut msg = format!(
        "do={} if={} fun={} => требуется end={}, фактически end={} (ДЕЛЬТА {}); repeat={} until={} (ДЕЛЬТА {})",
        n_do, n_if, n_fun, need_end, n_end, d_end, n_rep, n_unl, d_unl
    );
    if !regions.is_empty() {
        msg.push_str("\n    REGIONS(где могли быть проглочены end):");
        for (k, (sl, ty, snip, el)) in regions.iter().take(14).enumerate() {
            msg.push_str(&format!(
                "\n    [{}] {} стр.{}->{} : {:?}",
                k + 1,
                ty,
                sl,
                el,
                snip
            ));
        }
        if regions.len() > 14 {
            msg.push_str(&format!("\n    ... ещё {} регионов", regions.len() - 14));
        }
    }
    Some(msg)
}

// ==================== ВСПОМОГАТЕЛЬНЫЕ ====================

fn is_length_pos(prev: char, prev_word: &str, next: Option<char>) -> bool {
    let no = next
        .map(|d| {
            d.is_alphanumeric()
                || d == '_'
                || d == '('
                || d == '{'
                || d == '['
                || d == '"'
                || d == '\''
        })
        .unwrap_or(false);
    if !no {
        return false;
    }
    match prev {
        '=' | '(' | '[' | '{' | ',' | '+' | '-' | '*' | '/' | '%' | '^' | '<' | '>' | '~' => true,
        c if c.is_alphanumeric() || c == '_' => is_expr_keyword(prev_word),
        _ => false,
    }
}

fn is_expr_keyword(w: &str) -> bool {
    matches!(w, "return" | "and" | "or" | "not" | "in")
}

fn is_keyword(w: &str) -> bool {
    matches!(
        w,
        "then" | "else" | "elseif" | "end" | "until" | "do" | "function" | "return" | "if"
            | "while" | "for" | "repeat" | "local" | "in" | "and" | "or" | "not"
    )
}

fn sanitize_bitops(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut ins: Option<char> = None;
    let mut lc = false;
    let mut prev = '\0';
    let mut esc = false;
    for c in s.chars() {
        if lc {
            out.push(c);
            if c == '\n' {
                lc = false;
            }
            prev = c;
            continue;
        }
        if let Some(q) = ins {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == q {
                ins = None;
            }
            prev = c;
            continue;
        }
        match c {
            '\'' | '"' => {
                ins = Some(c);
                out.push(c);
            }
            '-' if prev == '-' => {
                lc = true;
                out.push(c);
            }
            '&' | '|' => out.push('+'),
            _ => out.push(c),
        }
        prev = c;
    }
    out
}

fn scan_left(out: &str) -> usize {
    let by = out.as_bytes();
    let t = by.len();
    let mut p = t;
    while p > 0
        && (by[p - 1] == b' ' || by[p - 1] == b'\t' || by[p - 1] == b'\n' || by[p - 1] == b'\r')
    {
        p -= 1;
    }
    if p == 0 {
        return t;
    }
    let ch = by[p - 1] as char;
    if ch == ')' || ch == ']' {
        let mut d = 0;
        let mut j = p;
        while j > 0 {
            let cj = by[j - 1] as char;
            if cj == ')' || cj == ']' {
                d += 1;
            } else if cj == '(' || cj == '[' {
                d -= 1;
                if d == 0 {
                    j -= 1;
                    break;
                }
            }
            j -= 1;
        }
        p = j;
        while p > 0 {
            let cj = by[p - 1] as char;
            if cj.is_alphanumeric() || cj == '_' || cj == '.' || cj == ':' {
                p -= 1;
            } else {
                break;
            }
        }
        p
    } else if ch.is_alphanumeric() || ch == '_' || ch == '.' {
        while p > 0 {
            let cj = by[p - 1] as char;
            if cj.is_alphanumeric() || cj == '_' || cj == '.' {
                p -= 1;
            } else {
                break;
            }
        }
        p
    } else {
        t
    }
}

fn scan_right(b: &[char], start: usize) -> usize {
    let n = b.len();
    if start >= n {
        return start;
    }
    let c0 = b[start];
    if c0 == '(' {
        let mut d = 0;
        let mut j = start;
        while j < n {
            if b[j] == '(' {
                d += 1;
            } else if b[j] == ')' {
                d -= 1;
                if d == 0 {
                    return j + 1;
                }
            }
            j += 1;
        }
        return j;
    }
    if c0 == '\'' || c0 == '"' {
        let mut j = start + 1;
        while j < n {
            if b[j] == '\\' {
                j += 2;
                continue;
            }
            if b[j] == c0 {
                return j + 1;
            }
            j += 1;
        }
        return j;
    }
    if c0 == '-' || c0 == '#' {
        return scan_right(b, start + 1);
    }
    let mut j = start;
    while j < n && (b[j].is_alphanumeric() || b[j] == '_' || b[j] == '.' || b[j] == ':') {
        j += 1;
    }
    j
}

fn next_nonspace(b: &[char], i: usize) -> Option<char> {
    let mut j = i;
    while j < b.len() && (b[j] == ' ' || b[j] == '\t' || b[j] == '\n' || b[j] == '\r') {
        j += 1;
    }
    b.get(j).copied()
}

fn long_open(b: &[char], i: usize) -> (usize, usize) {
    let mut j = i + 1;
    let mut eq = 0;
    while j < b.len() && b[j] == '=' {
        eq += 1;
        j += 1;
    }
    if j < b.len() && b[j] == '[' {
        (eq, j + 1 - i)
    } else {
        (0, 0)
    }
}

fn closes(b: &[char], i: usize, eq: usize) -> bool {
    let mut j = i + 1;
    for _ in 0..eq {
        if j >= b.len() || b[j] != '=' {
            return false;
        }
        j += 1;
    }
    j < b.len() && b[j] == ']'
}

fn seg(b: &[char], i: usize, len: usize) -> String {
    b[i..i + len].iter().collect()
}

fn copy_until_close(
    b: &[char],
    i: &mut usize,
    out: &mut String,
    eq: usize,
    olen: usize,
    line: &mut usize,
) {
    out.push_str(&seg(b, *i, olen));
    *i += olen;
    while *i < b.len() {
        if b[*i] == '\n' {
            *line += 1;
        }
        if b[*i] == ']' && closes(b, *i, eq) {
            let cl = eq + 2;
            out.push_str(&seg(b, *i, cl));
            *i += cl;
            return;
        }
        out.push(b[*i]);
        *i += 1;
    }
}