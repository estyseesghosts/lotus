#include <3ds.h>
#include "step.h"

static volatile unsigned value;

int main(void) {
    while (aptMainLoop()) {
        value += STEP_VALUE;
        gspWaitForVBlank();
    }
    return 0;
}
