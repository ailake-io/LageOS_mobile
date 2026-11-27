/* Stub de libxkbcommon: o crate xkbcommon (dependência do Seat do Smithay) referencia estes
 * símbolos nos Drop, mas o lange-compositor não cria teclado, então nunca são chamados.
 * Se forem, é bug: trap em vez de comportamento indefinido. Some quando o input usar xkb real. */
void xkb_context_unref(void *p) { (void)p; __builtin_trap(); }
void xkb_keymap_unref(void *p)  { (void)p; __builtin_trap(); }
void xkb_state_unref(void *p)   { (void)p; __builtin_trap(); }
