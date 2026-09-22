/* A minimal 64-bit Windows console program, for exercising the PE loader.
 *
 * No CRT: -nostdlib means there is no startup code, no __main, and the entry
 * point is `entry` as named on the link line.  All it does is call two
 * imported kernel32 functions, which is exactly what the loader has to
 * support -- parse the import table, bind the thunks, and survive the call.
 */
typedef void *HANDLE;

__declspec(dllimport) HANDLE __stdcall GetStdHandle(unsigned long nStdHandle);
__declspec(dllimport) int __stdcall WriteFile(HANDLE h, const void *buf,
                                              unsigned long n,
                                              unsigned long *written,
                                              void *overlapped);
__declspec(dllimport) void __stdcall ExitProcess(unsigned code);

void entry(void)
{
    static const char msg[] = "hello from a PE executable\r\n";
    unsigned long written = 0;
    HANDLE out = GetStdHandle((unsigned long)-11); /* STD_OUTPUT_HANDLE */
    WriteFile(out, msg, sizeof(msg) - 1, &written, 0);
    ExitProcess(0);
}
