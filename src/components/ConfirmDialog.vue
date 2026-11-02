<script setup lang="ts">
import { confirmState, resolveConfirm } from "../confirm";

function onMask() {
  resolveConfirm(false);
}
</script>

<template>
  <div v-if="confirmState" class="modal-mask" @click.self="onMask">
    <div class="modal">
      <div class="modal-title">{{ confirmState.title }}</div>
      <div class="modal-body">{{ confirmState.message }}</div>
      <div class="modal-actions">
        <button @click="resolveConfirm(false)">取消</button>
        <button
          :class="confirmState.danger ? 'danger-solid' : 'primary'"
          @click="resolveConfirm(true)"
        >
          {{ confirmState.confirmText }}
        </button>
      </div>
    </div>
  </div>
</template>
