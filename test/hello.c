/* A minimal 64-bit Windows console program, for exercising the PE loader.
 *
 * No CRT: -nostdlib means there is no startup code, no __main, and the entry
 * point is `entry` as named on the link line.  All it does is call two
 * imported kernel32 functions, which is exactly what the loader has to
 * support -- parse the import table, bind the thunks, and survive the call.
 *
 * One deliberate detail: `gp_msg` is an *absolute pointer* to the message,
 * stored in .data rather than reached RIP-relative.  Its initial value is an
 * address inside the image, so the linker must emit a base relocation for it.
 *
 * That matters twice over.  The loader maps the image at a base of its own
 * choosing, so an image with no relocation directory is refused outright --
 * and this file is what proves the loader's relocation step is not dead code.
 * If DIR64 fixups were skipped, `gp_msg` would still hold the linker's
 * address and the WriteFile below would print nothing (or fault).
 */
typedef void *HANDLE;

__declspec(dllimport) HANDLE __stdcall GetStdHandle(unsigned long nStdHandle);
__declspec(dllimport) int __stdcall WriteFile(HANDLE h, const void *buf,
                                              unsigned long n,
                                              unsigned long *written,
                                              void *overlapped);
__declspec(dllimport) void __stdcall ExitProcess(unsigned code);

static const char msg[] = "hello from a PE executable\r\n";

/* `volatile` is load-bearing: without it gcc constant-folds the pointer back
 * to `msg`, reaches it RIP-relative, and the image ends up position
 * independent -- with no .reloc at all.  The qualifier forces a real load from
 * this slot, so the slot must hold a real absolute address, so the linker must
 * emit a DIR64 fixup for it. */
static const char *volatile gp_msg = msg;

void entry(void)
{
    unsigned long written = 0;
    HANDLE out = GetStdHandle((unsigned long)-11); /* STD_OUTPUT_HANDLE */
    WriteFile(out, gp_msg, sizeof(msg) - 1, &written, 0);
    ExitProcess(0);
}
